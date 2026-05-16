//! Per-handler CORS policy registry for the Sky gateway.
//!
//! Built at startup from the manifest by scanning each handler's `middleware`
//! array for entries with `kind = "native"` and `name = "cors"`. The registry
//! is then used at request time to:
//!
//!   - Return preflight (OPTIONS) responses without forwarding to the worker.
//!   - Inject `Access-Control-*` headers into actual responses.

use crate::manifest::Manifest;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::HashMap;

// ── Policy ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CorsPolicy {
    /// Allowed origins. `["*"]` means any origin.
    pub origins: Vec<String>,
    pub credentials: bool,
    pub max_age: u64,
    pub allow_headers: Vec<String>,
    pub expose_headers: Vec<String>,
}

pub fn parse_cors_config(config: &Value) -> CorsPolicy {
    let origins = config
        .get("origins")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["*".to_string()]);

    let credentials = config
        .get("credentials")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let max_age = config
        .get("maxAge")
        .and_then(|v| v.as_u64())
        .unwrap_or(86_400);

    let allow_headers = config
        .get("allowHeaders")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["content-type".to_string(), "authorization".to_string()]);

    let expose_headers = config
        .get("exposeHeaders")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    CorsPolicy {
        origins,
        credentials,
        max_age,
        allow_headers,
        expose_headers,
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

struct PathCors {
    policy: CorsPolicy,
    /// All HTTP methods at this path that carry a CORS policy (for Allow-Methods in preflights).
    allow_methods: Vec<String>,
}

#[derive(Default)]
pub struct CorsRegistry {
    by_handler: HashMap<String, CorsPolicy>,
    by_path: HashMap<String, PathCors>,
}

impl CorsRegistry {
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut registry = Self::default();

        for service in &manifest.services {
            let prefix = service
                .group
                .as_ref()
                .map(|g| g.prefix.as_str())
                .unwrap_or("");

            for handler in &service.handlers {
                let Some(cors_mw) = handler
                    .middleware
                    .iter()
                    .find(|m| m.kind == "native" && m.name == "cors")
                else {
                    continue;
                };

                let config = cors_mw.config.as_ref().cloned().unwrap_or(Value::Null);
                let policy = parse_cors_config(&config);
                let handler_id = format!("{}.{}", service.class_name, handler.name);
                let full_path = format!("{}{}", prefix, handler.path);

                registry.by_handler.insert(handler_id, policy.clone());

                let path_entry = registry
                    .by_path
                    .entry(full_path)
                    .or_insert_with(|| PathCors {
                        policy: policy.clone(),
                        allow_methods: Vec::new(),
                    });
                path_entry.allow_methods.push(handler.method.to_uppercase());
            }
        }

        registry
    }

    pub fn get_by_handler(&self, handler_id: &str) -> Option<&CorsPolicy> {
        self.by_handler.get(handler_id)
    }

    /// Build and return a preflight response for the given path + request origin.
    /// Returns `None` if the path has no CORS policy registered.
    pub fn preflight(&self, path: &str, origin: &str) -> Option<Response> {
        let pc = self.by_path.get(path)?;
        Some(build_preflight(&pc.policy, &pc.allow_methods, origin))
    }

    #[allow(dead_code)]
    pub fn has_path(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }
}

// ── Response helpers ──────────────────────────────────────────────────────────

fn build_preflight(policy: &CorsPolicy, allow_methods: &[String], origin: &str) -> Response {
    let mut headers = HeaderMap::new();

    if let Some(resolved) = resolve_origin(policy, origin) {
        set_header(&mut headers, header::ACCESS_CONTROL_ALLOW_ORIGIN, &resolved);

        if policy.credentials {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }

        let methods = allow_methods.join(", ");
        set_header(&mut headers, header::ACCESS_CONTROL_ALLOW_METHODS, &methods);

        let hdrs = policy.allow_headers.join(", ");
        set_header(&mut headers, header::ACCESS_CONTROL_ALLOW_HEADERS, &hdrs);

        set_header(
            &mut headers,
            header::ACCESS_CONTROL_MAX_AGE,
            &policy.max_age.to_string(),
        );

        if resolved != "*" {
            headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        }
    }

    (StatusCode::NO_CONTENT, headers).into_response()
}

/// Inject CORS headers into a response's `HeaderMap` for actual (non-preflight) requests.
pub fn apply_cors_headers(policy: &CorsPolicy, origin: &str, headers: &mut HeaderMap) {
    let Some(resolved) = resolve_origin(policy, origin) else {
        return;
    };

    set_header(headers, header::ACCESS_CONTROL_ALLOW_ORIGIN, &resolved);

    if policy.credentials {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }

    if !policy.expose_headers.is_empty() {
        let expose = policy.expose_headers.join(", ");
        set_header(headers, header::ACCESS_CONTROL_EXPOSE_HEADERS, &expose);
    }

    if resolved != "*" {
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn resolve_origin(policy: &CorsPolicy, origin: &str) -> Option<String> {
    if policy.origins.iter().any(|o| o == "*") {
        if policy.credentials {
            // Wildcard + credentials is invalid per spec — echo the specific origin instead.
            Some(origin.to_string())
        } else {
            Some("*".to_string())
        }
    } else if policy.origins.iter().any(|o| o == origin) {
        Some(origin.to_string())
    } else {
        None
    }
}

fn set_header(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name, v);
    }
}
