//! JWT authentication middleware for the Sky gateway.
//!
//! Reads the manifest at startup to determine which routes require auth,
//! verifies HS256 JWTs on inbound requests, and forwards verified claims
//! to the worker as an `x-sky-claims` header in the INVOKE payload.
//!
//! Usage in sky.toml:
//! ```toml
//! [auth]
//! jwt_secret = "change-me-use-a-long-random-string"
//! ```
//!
//! Usage in TypeScript:
//! ```typescript
//! @Handler({ method: "GET", path: "/me", middleware: [requireAuth()] })
//! async getMe(@Header("x-sky-claims") claims: string) { ... }
//! ```

use crate::manifest::Manifest;
use axum::http::{HeaderMap, StatusCode};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde_json::Value;
use std::collections::HashMap;

// ── Per-handler auth config ───────────────────────────────────────────────────

struct AuthHandlerConfig {
    /// OAuth-style scopes the token must include. Empty = no scope check.
    scopes: Vec<String>,
    /// If true, missing/invalid tokens are not rejected — the handler runs
    /// without claims. A valid token is still verified and forwarded.
    optional: bool,
}

fn parse_auth_handler_config(config: Option<&Value>) -> AuthHandlerConfig {
    let Some(obj) = config.and_then(|v| v.as_object()) else {
        return AuthHandlerConfig {
            scopes: vec![],
            optional: false,
        };
    };

    let scopes = obj
        .get("scopes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let optional = obj
        .get("optional")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    AuthHandlerConfig { scopes, optional }
}

// ── Outcome ───────────────────────────────────────────────────────────────────

pub enum AuthOutcome {
    /// Token verified; `claims` is the full JWT payload.
    Allowed { claims: Value },
    /// This route does not require auth.
    NotRequired,
    /// Auth failed; return this HTTP response to the client.
    Denied {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

// ── Validator ─────────────────────────────────────────────────────────────────

pub struct AuthValidator {
    decoding_key: Option<DecodingKey>,
    /// handler_id ("ClassName.method") → Some(config) if auth required, None if not.
    handlers: HashMap<String, Option<AuthHandlerConfig>>,
}

impl AuthValidator {
    /// Build an `AuthValidator` from the manifest and the configured JWT secret.
    ///
    /// Returns `Err` if any handler declares `requireAuth` but no secret is set —
    /// this fails gateway startup rather than silently disabling auth.
    pub fn from_manifest(manifest: &Manifest, jwt_secret: &str) -> Result<Self, String> {
        let mut handlers: HashMap<String, Option<AuthHandlerConfig>> = HashMap::new();

        for service in &manifest.services {
            // Check for service-level auth middleware (applies to all handlers).
            let service_auth = service
                .middleware
                .iter()
                .find(|m| m.kind == "native" && m.name == "auth");

            for handler in &service.handlers {
                let handler_id = format!("{}.{}", service.class_name, handler.name);

                // Handler-level auth overrides service-level.
                let auth_entry = handler
                    .middleware
                    .iter()
                    .find(|m| m.kind == "native" && m.name == "auth")
                    .or(service_auth);

                if let Some(entry) = auth_entry {
                    if jwt_secret.is_empty() {
                        return Err(format!(
                            "handler '{handler_id}' declares requireAuth() but [auth].jwt_secret is not set in sky.toml"
                        ));
                    }
                    let config = parse_auth_handler_config(entry.config.as_ref());
                    handlers.insert(handler_id, Some(config));
                } else {
                    handlers.insert(handler_id, None);
                }
            }
        }

        let decoding_key = if jwt_secret.is_empty() {
            None
        } else {
            Some(DecodingKey::from_secret(jwt_secret.as_bytes()))
        };

        Ok(Self {
            decoding_key,
            handlers,
        })
    }

    /// Check auth for the given handler against the incoming request headers.
    pub fn check(&self, handler_id: &str, headers: &HeaderMap) -> AuthOutcome {
        let Some(auth_config) = self.handlers.get(handler_id) else {
            return AuthOutcome::NotRequired;
        };

        let Some(auth_config) = auth_config else {
            return AuthOutcome::NotRequired;
        };

        let Some(key) = &self.decoding_key else {
            // Should never happen — from_manifest() rejects this combination.
            return AuthOutcome::Denied {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "auth_misconfigured",
                message: "JWT secret is not configured".to_string(),
            };
        };

        let token = match extract_bearer(headers) {
            Some(t) => t,
            None if auth_config.optional => return AuthOutcome::NotRequired,
            None => {
                return AuthOutcome::Denied {
                    status: StatusCode::UNAUTHORIZED,
                    code: "jwt_missing",
                    message: "Authorization header with Bearer token is required".to_string(),
                };
            }
        };

        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        validation.validate_aud = false;

        match decode::<Value>(token, key, &validation) {
            Ok(data) => {
                if let Some(reason) = check_scopes(&data.claims, &auth_config.scopes) {
                    return AuthOutcome::Denied {
                        status: StatusCode::FORBIDDEN,
                        code: "insufficient_scopes",
                        message: reason,
                    };
                }
                AuthOutcome::Allowed {
                    claims: data.claims,
                }
            }
            Err(e) => AuthOutcome::Denied {
                status: StatusCode::UNAUTHORIZED,
                code: "jwt_invalid",
                message: format!("Invalid or expired token: {e}"),
            },
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?;

    value.strip_prefix("Bearer ")
}

/// Returns `Some(error_message)` if any required scope is missing.
pub(crate) fn check_scopes(claims: &Value, required: &[String]) -> Option<String> {
    // Accept either `scope` (space-separated string) or `scopes` (array).
    let token_scopes: Vec<String> =
        if let Some(scope_str) = claims.get("scope").and_then(|v| v.as_str()) {
            scope_str
                .split_whitespace()
                .map(|s| s.to_string())
                .collect()
        } else if let Some(arr) = claims.get("scopes").and_then(|v| v.as_array()) {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        } else {
            vec![]
        };

    for required_scope in required {
        if !token_scopes.contains(required_scope) {
            return Some(format!("Token lacks required scope: '{required_scope}'"));
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use axum::http::header::AUTHORIZATION;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::time::{SystemTime, UNIX_EPOCH};

    const SECRET: &str = "test-secret-key";

    // ── Token helpers ────────────────────────────────────────────────────────

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn mint(secret: &str, claims: serde_json::Value) -> String {
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    fn valid_token() -> String {
        mint(
            SECRET,
            serde_json::json!({ "sub": "user-1", "exp": now() + 3600 }),
        )
    }

    fn expired_token() -> String {
        // Use a leeway-safe past time (100 days ago).
        mint(
            SECRET,
            serde_json::json!({ "sub": "user-1", "exp": now() - 86400 * 100 }),
        )
    }

    fn scoped_token_array(scopes: &[&str]) -> String {
        mint(
            SECRET,
            serde_json::json!({ "sub": "user-1", "exp": now() + 3600, "scopes": scopes }),
        )
    }

    fn scoped_token_string(scope: &str) -> String {
        mint(
            SECRET,
            serde_json::json!({ "sub": "user-1", "exp": now() + 3600, "scope": scope }),
        )
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        h
    }

    fn no_headers() -> HeaderMap {
        HeaderMap::new()
    }

    // ── Manifest fixtures ────────────────────────────────────────────────────

    fn manifest_no_auth() -> Manifest {
        Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "open", "method": "GET", "path": "/open",
                        "status": 200, "validate": false, "extract": []
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn manifest_with_auth_handler() -> Manifest {
        Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [
                        {
                            "name": "protected", "method": "GET", "path": "/me",
                            "status": 200, "validate": false, "extract": [],
                            "middleware": [{ "kind": "native", "name": "auth", "config": {} }]
                        },
                        {
                            "name": "open", "method": "GET", "path": "/open",
                            "status": 200, "validate": false, "extract": []
                        }
                    ]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn manifest_with_scoped_handler(scopes: &[&str]) -> Manifest {
        Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "admin", "method": "GET", "path": "/admin",
                        "status": 200, "validate": false, "extract": [],
                        "middleware": [{
                            "kind": "native", "name": "auth",
                            "config": { "scopes": scopes }
                        }]
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn manifest_service_level_auth() -> Manifest {
        Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "middleware": [{ "kind": "native", "name": "auth", "config": {} }],
                    "handlers": [
                        {
                            "name": "handlerA", "method": "GET", "path": "/a",
                            "status": 200, "validate": false, "extract": []
                        },
                        {
                            "name": "handlerB", "method": "GET", "path": "/b",
                            "status": 200, "validate": false, "extract": []
                        }
                    ]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn manifest_handler_overrides_service_auth() -> Manifest {
        // Service requires auth; one handler declares optional=true.
        Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "middleware": [{ "kind": "native", "name": "auth", "config": {} }],
                    "handlers": [{
                        "name": "optHandler", "method": "GET", "path": "/opt",
                        "status": 200, "validate": false, "extract": [],
                        "middleware": [{
                            "kind": "native", "name": "auth",
                            "config": { "optional": true }
                        }]
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap()
    }

    // ── from_manifest ────────────────────────────────────────────────────────

    #[test]
    fn empty_secret_accepted_when_no_auth_routes() {
        let v = AuthValidator::from_manifest(&manifest_no_auth(), "");
        assert!(v.is_ok());
    }

    #[test]
    fn empty_secret_rejected_when_auth_route_exists() {
        let err = AuthValidator::from_manifest(&manifest_with_auth_handler(), "")
            .err()
            .expect("expected Err when secret is empty and auth route exists");
        assert!(
            err.contains("jwt_secret"),
            "error should mention jwt_secret: {err}"
        );
        assert!(
            err.contains("Svc.protected"),
            "error should name the handler: {err}"
        );
    }

    #[test]
    fn secret_and_auth_route_succeeds() {
        assert!(AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).is_ok());
    }

    #[test]
    fn service_level_auth_without_secret_fails() {
        let err = AuthValidator::from_manifest(&manifest_service_level_auth(), "")
            .err()
            .expect("expected Err when secret is empty and service-level auth exists");
        assert!(err.contains("jwt_secret"));
    }

    // ── check: unprotected routes ────────────────────────────────────────────

    #[test]
    fn not_required_for_unknown_handler() {
        let v = AuthValidator::from_manifest(&manifest_no_auth(), "").unwrap();
        assert!(matches!(
            v.check("Nobody.nothing", &no_headers()),
            AuthOutcome::NotRequired
        ));
    }

    #[test]
    fn not_required_for_unprotected_handler() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        assert!(matches!(
            v.check("Svc.open", &no_headers()),
            AuthOutcome::NotRequired
        ));
    }

    // ── check: missing / malformed token ─────────────────────────────────────

    #[test]
    fn denied_jwt_missing_when_no_authorization_header() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        match v.check("Svc.protected", &no_headers()) {
            AuthOutcome::Denied { code, status, .. } => {
                assert_eq!(code, "jwt_missing");
                assert_eq!(status, StatusCode::UNAUTHORIZED);
            }
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    #[test]
    fn denied_jwt_invalid_when_token_is_garbage() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, "Bearer not.a.jwt".parse().unwrap());
        match v.check("Svc.protected", &h) {
            AuthOutcome::Denied { code, status, .. } => {
                assert_eq!(code, "jwt_invalid");
                assert_eq!(status, StatusCode::UNAUTHORIZED);
            }
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    #[test]
    fn denied_jwt_invalid_when_signed_with_wrong_secret() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        let token = mint(
            "wrong-secret",
            serde_json::json!({ "sub": "u", "exp": now() + 3600 }),
        );
        match v.check("Svc.protected", &bearer(&token)) {
            AuthOutcome::Denied { code, .. } => assert_eq!(code, "jwt_invalid"),
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    #[test]
    fn denied_jwt_invalid_when_token_is_expired() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        match v.check("Svc.protected", &bearer(&expired_token())) {
            AuthOutcome::Denied { code, status, .. } => {
                assert_eq!(code, "jwt_invalid");
                assert_eq!(status, StatusCode::UNAUTHORIZED);
            }
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    // ── check: valid token ───────────────────────────────────────────────────

    #[test]
    fn allowed_with_valid_token_and_claims_forwarded() {
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        match v.check("Svc.protected", &bearer(&valid_token())) {
            AuthOutcome::Allowed { claims } => {
                assert_eq!(claims["sub"], "user-1");
            }
            other => panic!("expected Allowed, got {}", variant_name(&other)),
        }
    }

    #[test]
    fn allowed_with_aud_claim_in_token() {
        // Tokens that include `aud` must not be rejected — we don't validate audience.
        let v = AuthValidator::from_manifest(&manifest_with_auth_handler(), SECRET).unwrap();
        let token = mint(
            SECRET,
            serde_json::json!({ "sub": "u", "exp": now() + 3600, "aud": "sky-api" }),
        );
        assert!(matches!(
            v.check("Svc.protected", &bearer(&token)),
            AuthOutcome::Allowed { .. }
        ));
    }

    // ── check: scope enforcement ─────────────────────────────────────────────

    #[test]
    fn allowed_when_token_scope_array_contains_required_scope() {
        let v = AuthValidator::from_manifest(&manifest_with_scoped_handler(&["admin"]), SECRET)
            .unwrap();
        assert!(matches!(
            v.check(
                "Svc.admin",
                &bearer(&scoped_token_array(&["admin", "read"]))
            ),
            AuthOutcome::Allowed { .. }
        ));
    }

    #[test]
    fn allowed_when_token_scope_string_contains_required_scope() {
        let v = AuthValidator::from_manifest(&manifest_with_scoped_handler(&["write"]), SECRET)
            .unwrap();
        assert!(matches!(
            v.check(
                "Svc.admin",
                &bearer(&scoped_token_string("read write admin"))
            ),
            AuthOutcome::Allowed { .. }
        ));
    }

    #[test]
    fn denied_when_token_scope_array_lacks_required_scope() {
        let v = AuthValidator::from_manifest(&manifest_with_scoped_handler(&["admin"]), SECRET)
            .unwrap();
        match v.check("Svc.admin", &bearer(&scoped_token_array(&["read"]))) {
            AuthOutcome::Denied { code, status, .. } => {
                assert_eq!(code, "insufficient_scopes");
                assert_eq!(status, StatusCode::FORBIDDEN);
            }
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    #[test]
    fn denied_when_token_has_no_scopes_but_scopes_required() {
        let v = AuthValidator::from_manifest(&manifest_with_scoped_handler(&["admin"]), SECRET)
            .unwrap();
        match v.check("Svc.admin", &bearer(&valid_token())) {
            AuthOutcome::Denied { code, .. } => assert_eq!(code, "insufficient_scopes"),
            other => panic!("expected Denied, got {}", variant_name(&other)),
        }
    }

    // ── check: optional auth ─────────────────────────────────────────────────

    #[test]
    fn optional_auth_allows_missing_token() {
        let manifest = Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "feed", "method": "GET", "path": "/feed",
                        "status": 200, "validate": false, "extract": [],
                        "middleware": [{
                            "kind": "native", "name": "auth",
                            "config": { "optional": true }
                        }]
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap();
        let v = AuthValidator::from_manifest(&manifest, SECRET).unwrap();
        assert!(matches!(
            v.check("Svc.feed", &no_headers()),
            AuthOutcome::NotRequired
        ));
    }

    #[test]
    fn optional_auth_allows_valid_token() {
        let manifest = Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "feed", "method": "GET", "path": "/feed",
                        "status": 200, "validate": false, "extract": [],
                        "middleware": [{
                            "kind": "native", "name": "auth",
                            "config": { "optional": true }
                        }]
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap();
        let v = AuthValidator::from_manifest(&manifest, SECRET).unwrap();
        assert!(matches!(
            v.check("Svc.feed", &bearer(&valid_token())),
            AuthOutcome::Allowed { .. }
        ));
    }

    #[test]
    fn optional_auth_still_rejects_invalid_token() {
        let manifest = Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{
                    "name": "svc", "className": "Svc", "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "feed", "method": "GET", "path": "/feed",
                        "status": 200, "validate": false, "extract": [],
                        "middleware": [{
                            "kind": "native", "name": "auth",
                            "config": { "optional": true }
                        }]
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap();
        let v = AuthValidator::from_manifest(&manifest, SECRET).unwrap();
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, "Bearer bad.token".parse().unwrap());
        assert!(matches!(
            v.check("Svc.feed", &h),
            AuthOutcome::Denied { .. }
        ));
    }

    // ── check: service-level auth ────────────────────────────────────────────

    #[test]
    fn service_level_auth_protects_all_handlers() {
        let v = AuthValidator::from_manifest(&manifest_service_level_auth(), SECRET).unwrap();
        // Both handlers are protected.
        assert!(matches!(
            v.check("Svc.handlerA", &no_headers()),
            AuthOutcome::Denied {
                code: "jwt_missing",
                ..
            }
        ));
        assert!(matches!(
            v.check("Svc.handlerB", &no_headers()),
            AuthOutcome::Denied {
                code: "jwt_missing",
                ..
            }
        ));
        // Both allow a valid token.
        assert!(matches!(
            v.check("Svc.handlerA", &bearer(&valid_token())),
            AuthOutcome::Allowed { .. }
        ));
    }

    #[test]
    fn handler_level_auth_overrides_service_level() {
        // Service requires auth; handler declares optional=true.
        let v = AuthValidator::from_manifest(&manifest_handler_overrides_service_auth(), SECRET)
            .unwrap();
        // No token → NotRequired (optional) rather than Denied (service default).
        assert!(matches!(
            v.check("Svc.optHandler", &no_headers()),
            AuthOutcome::NotRequired
        ));
    }

    // ── check_scopes helper ──────────────────────────────────────────────────

    #[test]
    fn check_scopes_passes_when_required_list_empty() {
        let claims = serde_json::json!({});
        assert!(check_scopes(&claims, &[]).is_none());
    }

    #[test]
    fn check_scopes_passes_with_space_separated_string() {
        let claims = serde_json::json!({ "scope": "read write admin" });
        assert!(check_scopes(&claims, &["admin".to_string()]).is_none());
    }

    #[test]
    fn check_scopes_passes_with_array() {
        let claims = serde_json::json!({ "scopes": ["read", "admin"] });
        assert!(check_scopes(&claims, &["admin".to_string()]).is_none());
    }

    #[test]
    fn check_scopes_fails_with_missing_scope() {
        let claims = serde_json::json!({ "scopes": ["read"] });
        let err = check_scopes(&claims, &["admin".to_string()]).unwrap();
        assert!(
            err.contains("admin"),
            "error should name the missing scope: {err}"
        );
    }

    // ── Helper ───────────────────────────────────────────────────────────────

    fn variant_name(outcome: &AuthOutcome) -> &'static str {
        match outcome {
            AuthOutcome::Allowed { .. } => "Allowed",
            AuthOutcome::NotRequired => "NotRequired",
            AuthOutcome::Denied { .. } => "Denied",
        }
    }
}
