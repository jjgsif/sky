//! JSON Schema validation for the Sky gateway.
//!
//! At startup, this module compiles JSON Schemas from the manifest
//! into validators. During request handling, the gateway validates
//! incoming request bodies against the compiled schema before
//! dispatching to the worker. Invalid payloads are rejected with a
//! 400 response containing field-level error details.
//!
//! Handlers with `validate: false` skip validation entirely.

use crate::manifest::Manifest;
use serde::Serialize;
use std::collections::HashMap;
use thiserror::Error;

// ──────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────

/// Key for looking up a handler's compiled validator.
///
/// Uses service + handler name rather than method + path so that
/// the router can look up validators without re-resolving group
/// prefixes at request time.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct HandlerKey {
    pub service: String,
    pub handler: String,
}

impl std::fmt::Display for HandlerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", self.service, self.handler)
    }
}

/// The compiled validator store. Built once at startup, shared
/// (via `Arc`) across all request-handling tasks.
#[derive(Debug)]
pub struct SchemaRegistry {
    /// Compiled validators keyed by (service, handler).
    /// Only handlers with `validate: true` AND a body parameter
    /// with a schema are present.
    validators: HashMap<HandlerKey, CompiledValidator>,
}

/// Wraps a compiled JSON Schema validator alongside the raw schema
/// value (kept for diagnostics / error messages).
#[derive(Debug)]
struct CompiledValidator {
    /// The compiled validator from the `jsonschema` crate.
    validator: jsonschema::Validator,

    /// The raw schema, kept for debug logging.
    #[allow(dead_code)]
    raw_schema: serde_json::Value,
}

/// A single field-level validation error.
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    /// JSON Pointer path to the invalid field (e.g., "/email").
    pub path: String,

    /// Human-readable description of why validation failed.
    pub message: String,
}

/// The structured 400 response body for validation failures.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationErrorResponse {
    pub code: &'static str,
    pub message: String,
    pub errors: Vec<FieldError>,
}

// ──────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("failed to resolve $ref '{ref_path}' for handler {handler}")]
    UnresolvedRef { ref_path: String, handler: String },

    #[error("failed to compile schema for handler {handler}: {reason}")]
    CompileFailed { handler: String, reason: String },
}

// ──────────────────────────────────────────────
// Building the registry
// ──────────────────────────────────────────────

impl SchemaRegistry {
    /// Compile all handler body schemas from the manifest into
    /// validators.
    ///
    /// Iterates every service and handler. For handlers where
    /// `validate` is `true` and there is at least one `body`
    /// extract with a schema, resolves any `$ref` pointers and
    /// compiles the resolved schema into a `jsonschema::Validator`.
    ///
    /// Handlers with `validate: false` or without a body schema
    /// are skipped — lookups for them will return `None`.
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, SchemaError> {
        let mut validators = HashMap::new();

        for service in &manifest.services {
            for handler in &service.handlers {
                // Skip handlers that opted out of validation.
                if !handler.validate {
                    continue;
                }

                // Find the body parameter's schema, if any.
                let body_schema = handler
                    .extract
                    .iter()
                    .find(|e| e.source == "body")
                    .and_then(|e| e.schema.as_ref());

                let raw_schema = match body_schema {
                    Some(schema) => schema,
                    None => continue, // No body param or no schema — nothing to validate.
                };

                // Resolve $ref if present.
                let resolved = resolve_schema(
                    raw_schema,
                    manifest,
                    &format!("{}::{}", service.name, handler.name),
                )?;

                // Compile the schema.
                let key = HandlerKey {
                    service: service.name.clone(),
                    handler: handler.name.clone(),
                };

                let validator = jsonschema::validator_for(&resolved).map_err(|e| {
                    SchemaError::CompileFailed {
                        handler: key.to_string(),
                        reason: e.to_string(),
                    }
                })?;

                validators.insert(
                    key,
                    CompiledValidator {
                        validator,
                        raw_schema: resolved,
                    },
                );
            }
        }

        Ok(Self { validators })
    }

    /// Validate a JSON body against the schema for the given handler.
    ///
    /// Returns `Ok(())` if:
    /// - The handler has no compiled validator (opted out or no body schema).
    /// - The body passes validation.
    ///
    /// Returns `Err(ValidationErrorResponse)` if validation fails,
    /// with field-level error details suitable for a 400 response.
    pub fn validate(
        &self,
        key: &HandlerKey,
        body: &serde_json::Value,
    ) -> Result<(), ValidationErrorResponse> {
        let compiled = match self.validators.get(key) {
            Some(v) => v,
            None => return Ok(()), // No validator → pass through.
        };

        let result = compiled.validator.validate(body);

        if result.is_ok() {
            return Ok(());
        }

        // Collect all validation errors.
        let errors: Vec<FieldError> = compiled
            .validator
            .iter_errors(body)
            .map(|err| {
                let path = format!("/{}", err.instance_path());
                // Clean up the root path case.
                let path = if path == "/" { String::new() } else { path };

                FieldError {
                    path,
                    message: err.to_string(),
                }
            })
            .collect();

        Err(ValidationErrorResponse {
            code: "validation_failed",
            message: format!(
                "Request body failed schema validation ({} error{})",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            ),
            errors,
        })
    }

    /// Check whether a handler has a compiled validator.
    ///
    /// Useful for the router to decide whether to parse the body
    /// as JSON at all (e.g., GET requests with no body).
    #[allow(dead_code)]
    pub fn has_validator(&self, key: &HandlerKey) -> bool {
        self.validators.contains_key(key)
    }

    /// Number of compiled validators (for diagnostics / tracing).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.validators.len()
    }

    /// Whether the registry has no validators at all.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }
}

// ──────────────────────────────────────────────
// Schema resolution
// ──────────────────────────────────────────────

/// Resolve a schema value, replacing top-level `$ref` with the
/// actual schema definition from the manifest.
///
/// If the schema is a `$ref`, look it up in `manifest.schemas`.
/// If it's an inline schema, return it as-is.
///
/// This handles the common case where the emitter produces
/// `{ "$ref": "#/schemas/CreateUserRequest" }` as the body schema.
/// Nested `$ref`s within properties are resolved by building a
/// combined document with `$defs` so the `jsonschema` crate can
/// resolve them internally.
fn resolve_schema(
    schema: &serde_json::Value,
    manifest: &Manifest,
    handler_label: &str,
) -> Result<serde_json::Value, SchemaError> {
    // If the top-level schema is a $ref, resolve it.
    if let Some(ref_path) = schema.get("$ref").and_then(|v| v.as_str()) {
        let resolved =
            manifest
                .resolve_schema(ref_path)
                .ok_or_else(|| SchemaError::UnresolvedRef {
                    ref_path: ref_path.to_string(),
                    handler: handler_label.to_string(),
                })?;

        // Build a self-contained document with $defs for any
        // schemas that might be referenced within the resolved schema.
        return build_self_contained(resolved.clone(), manifest);
    }

    // Inline schema — still might contain nested $refs in properties,
    // so wrap with $defs.
    build_self_contained(schema.clone(), manifest)
}

/// Build a self-contained JSON Schema document by attaching all
/// manifest schemas as `$defs`. This lets the `jsonschema` crate
/// resolve any `$ref` pointers within nested properties without
/// us needing to walk and inline them manually.
fn build_self_contained(
    mut schema: serde_json::Value,
    manifest: &Manifest,
) -> Result<serde_json::Value, SchemaError> {
    if manifest.schemas.is_empty() {
        return Ok(schema);
    }

    // Only add $defs if the schema is an object (which it should be
    // for any meaningful body schema).
    if let serde_json::Value::Object(ref mut map) = schema {
        // Convert manifest schemas into $defs format.
        // Manifest stores schemas under flat names ("CreateUserRequest"),
        // and $refs point to "#/schemas/CreateUserRequest".
        // We remap $refs to "#/$defs/CreateUserRequest" and store
        // schemas under $defs.
        let defs: serde_json::Map<String, serde_json::Value> = manifest
            .schemas
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        map.insert("$defs".to_string(), serde_json::Value::Object(defs));

        // Rewrite $ref paths from #/schemas/X to #/$defs/X throughout
        // the document so the jsonschema crate can resolve them.
        let rewritten = rewrite_refs(&serde_json::Value::Object(map.clone()));
        return Ok(rewritten);
    }

    Ok(schema)
}

/// Recursively rewrite `$ref` values from `#/schemas/X` to `#/$defs/X`.
fn rewrite_refs(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                if k == "$ref" {
                    if let serde_json::Value::String(ref_path) = v {
                        let rewritten = ref_path.replace("#/schemas/", "#/$defs/");
                        new_map.insert(k.clone(), serde_json::Value::String(rewritten));
                    } else {
                        new_map.insert(k.clone(), v.clone());
                    }
                } else {
                    new_map.insert(k.clone(), rewrite_refs(v));
                }
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(rewrite_refs).collect())
        }
        other => other.clone(),
    }
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    /// Helper: build a manifest with configurable handlers and schemas.
    fn test_manifest(handlers: serde_json::Value, schemas: serde_json::Value) -> Manifest {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "testService",
                "className": "TestService",
                "lifetime": "request",
                "dependencies": [],
                "handlers": handlers
            }],
            "middleware": [],
            "schemas": schemas
        });

        Manifest::from_json(&json.to_string()).unwrap()
    }

    // ── Registry building ───────────────────────

    #[test]
    fn compiles_inline_schema() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/items",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" }
                        },
                        "required": ["name"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        assert_eq!(registry.len(), 1);

        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };
        assert!(registry.has_validator(&key));
    }

    #[test]
    fn compiles_ref_schema() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/items",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": { "$ref": "#/schemas/CreateItem" }
                }]
            }]),
            serde_json::json!({
                "CreateItem": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "count": { "type": "integer" }
                    },
                    "required": ["name"]
                }
            }),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn skips_validate_false() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "get",
                "method": "GET",
                "path": "/items/:id",
                "status": 200,
                "validate": false,
                "extract": [
                    { "source": "param", "name": "id", "position": 0 }
                ]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn skips_handler_without_body() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "list",
                "method": "GET",
                "path": "/items",
                "status": 200,
                "validate": true,
                "extract": [
                    { "source": "query", "name": "page", "position": 0 }
                ]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn skips_body_without_schema() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "raw",
                "method": "POST",
                "path": "/raw",
                "status": 200,
                "validate": true,
                "extract": [
                    { "source": "body", "position": 0 }
                ]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn errors_on_unresolved_ref() {
        let manifest = Manifest {
            version: "1".into(),
            hash: String::new(),
            emitted_at: String::new(),
            services: vec![crate::manifest::ServiceDescriptor {
                name: "testService".into(),
                class_name: "TestService".into(),
                lifetime: "request".into(),
                dependencies: vec![],
                handlers: vec![crate::manifest::HandlerDescriptor {
                    name: "create".into(),
                    method: "POST".into(),
                    path: "/items".into(),
                    status: 201,
                    validate: true,
                    stream_response_body: false,
                    stream_request_body: false,
                    extract: vec![crate::manifest::ExtractDescriptor {
                        field: "body".into(),
                        source: "body".into(),
                        name: None,
                        schema: Some(serde_json::json!({ "$ref": "#/schemas/DoesNotExist" })),
                    }],
                    response: None,
                    middleware: vec![],
                    timeout_ms: None,
                }],
                group: None,
                middleware: vec![],
            }],
            middleware: vec![],
            schemas: HashMap::new(),
        };

        let err = SchemaRegistry::from_manifest(&manifest).unwrap_err();
        assert!(matches!(err, SchemaError::UnresolvedRef { .. }));
    }

    // ── Validation ──────────────────────────────

    #[test]
    fn validates_valid_body() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "email": { "type": "string" }
                        },
                        "required": ["name", "email"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        let body = serde_json::json!({
            "name": "Alice",
            "email": "alice@example.com"
        });

        assert!(registry.validate(&key, &body).is_ok());
    }

    #[test]
    fn rejects_missing_required_field() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "email": { "type": "string" }
                        },
                        "required": ["name", "email"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        // Missing "email"
        let body = serde_json::json!({ "name": "Alice" });
        let err = registry.validate(&key, &body).unwrap_err();

        assert_eq!(err.code, "validation_failed");
        assert!(!err.errors.is_empty());
        // Should mention the missing field.
        let has_email_error = err.errors.iter().any(|e| e.message.contains("email"));
        assert!(
            has_email_error,
            "expected error about 'email', got: {:?}",
            err.errors
        );
    }

    #[test]
    fn rejects_wrong_type() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "age": { "type": "integer" }
                        },
                        "required": ["age"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        // "age" should be integer, not string.
        let body = serde_json::json!({ "age": "twenty-five" });
        let err = registry.validate(&key, &body).unwrap_err();

        assert_eq!(err.code, "validation_failed");
        let has_age_error = err.errors.iter().any(|e| e.path.contains("age"));
        assert!(
            has_age_error,
            "expected error at /age, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn rejects_invalid_enum_value() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "role": { "type": "string", "enum": ["admin", "member"] }
                        },
                        "required": ["role"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        let body = serde_json::json!({ "role": "superuser" });
        let err = registry.validate(&key, &body).unwrap_err();
        assert!(!err.errors.is_empty());
    }

    #[test]
    fn passes_unknown_handler_key() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/items",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": { "type": "object" }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();

        // Unknown key → no validator → pass through.
        let key = HandlerKey {
            service: "noSuchService".into(),
            handler: "noSuchHandler".into(),
        };

        let body = serde_json::json!({ "anything": "goes" });
        assert!(registry.validate(&key, &body).is_ok());
    }

    #[test]
    fn multiple_errors_reported() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "age": { "type": "integer" },
                            "email": { "type": "string" }
                        },
                        "required": ["name", "age", "email"]
                    }
                }]
            }]),
            serde_json::json!({}),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        // Missing all three required fields.
        let body = serde_json::json!({});
        let err = registry.validate(&key, &body).unwrap_err();

        // Should report errors for all three missing fields.
        assert!(
            err.errors.len() >= 3,
            "expected at least 3 errors, got {}: {:?}",
            err.errors.len(),
            err.errors
        );
    }

    // ── $ref resolution with validation ─────────

    #[test]
    fn validates_via_ref_schema() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/users",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": { "$ref": "#/schemas/CreateUser" }
                }]
            }]),
            serde_json::json!({
                "CreateUser": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "email": { "type": "string" }
                    },
                    "required": ["name", "email"]
                }
            }),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        // Valid
        let valid = serde_json::json!({ "name": "Bob", "email": "bob@example.com" });
        assert!(registry.validate(&key, &valid).is_ok());

        // Invalid — missing email
        let invalid = serde_json::json!({ "name": "Bob" });
        assert!(registry.validate(&key, &invalid).is_err());
    }

    #[test]
    fn validates_nested_ref_in_properties() {
        let manifest = test_manifest(
            serde_json::json!([{
                "name": "create",
                "method": "POST",
                "path": "/orders",
                "status": 201,
                "validate": true,
                "extract": [{
                    "source": "body",
                    "position": 0,
                    "schema": { "$ref": "#/schemas/CreateOrder" }
                }]
            }]),
            serde_json::json!({
                "CreateOrder": {
                    "type": "object",
                    "properties": {
                        "item": { "$ref": "#/schemas/OrderItem" },
                        "quantity": { "type": "integer" }
                    },
                    "required": ["item", "quantity"]
                },
                "OrderItem": {
                    "type": "object",
                    "properties": {
                        "sku": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["sku"]
                }
            }),
        );

        let registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let key = HandlerKey {
            service: "testService".into(),
            handler: "create".into(),
        };

        // Valid — nested object satisfies OrderItem schema.
        let valid = serde_json::json!({
            "item": { "sku": "ABC-123", "name": "Widget" },
            "quantity": 5
        });
        assert!(registry.validate(&key, &valid).is_ok());

        // Invalid — nested "item" missing required "sku".
        let invalid = serde_json::json!({
            "item": { "name": "Widget" },
            "quantity": 5
        });
        let err = registry.validate(&key, &invalid).unwrap_err();
        assert!(!err.errors.is_empty());
    }

    // ── Error response structure ────────────────

    #[test]
    fn error_response_serializes_correctly() {
        let response = ValidationErrorResponse {
            code: "validation_failed",
            message: "Request body failed schema validation (1 error)".into(),
            errors: vec![FieldError {
                path: "/email".into(),
                message: "is a required property".into(),
            }],
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["code"], "validation_failed");
        assert!(json["errors"].is_array());
        assert_eq!(json["errors"][0]["path"], "/email");
    }
}
