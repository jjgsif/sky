//! Manifest types and loader for the Sky gateway.
//!
//! The TypeScript emitter (`sky build`) produces a `sky-manifest.json`
//! file describing every service, handler, middleware, route group,
//! and JSON Schema in the application. This module defines the Rust
//! types that mirror that JSON structure, a loader that reads and
//! validates the manifest at gateway startup, and helper methods used
//! by the router and validator subsystems.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

// ──────────────────────────────────────────────
// Error types
// ──────────────────────────────────────────────

/// Errors that can occur when loading or validating a manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to read manifest file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest JSON: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("unsupported manifest version '{found}'; expected '{expected}'")]
    UnsupportedVersion { found: String, expected: String },

    #[error("duplicate route: {method} {path}")]
    DuplicateRoute { method: String, path: String },

    #[error("unresolved schema $ref: {0}")]
    UnresolvedRef(String),

    #[error("handler '{handler}' on service '{service}' has duplicate extract field names: {details}")]
    DuplicateExtractField {
        service: String,
        handler: String,
        details: String,
    },
}

// ──────────────────────────────────────────────
// Top-level manifest
// ──────────────────────────────────────────────

/// The currently supported manifest version.
const MANIFEST_VERSION: &str = "1";

/// Top-level manifest produced by `sky build`.
///
/// This is the Rust-side representation of `sky-manifest.json`.
/// Every field maps 1:1 to the JSON the TypeScript emitter outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version string. Must be "1" for this release.
    pub version: String,

    /// Content hash of the source files used to produce this manifest.
    /// Used for change detection by `sky build --watch`.
    pub hash: String,

    /// ISO 8601 timestamp of when the manifest was emitted.
    pub emitted_at: String,

    /// All decorated service classes discovered by the emitter.
    pub services: Vec<ServiceDescriptor>,

    /// All middleware classes (global and scoped).
    pub middleware: Vec<MiddlewareDescriptor>,

    /// Named JSON Schema definitions, referenced via `$ref` from
    /// handler parameters and responses.
    pub schemas: HashMap<String, serde_json::Value>,
}

// ──────────────────────────────────────────────
// Service & handler descriptors
// ──────────────────────────────────────────────

/// A decorated `@Service` class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    /// Service name (typically the class name, lowercased).
    pub name: String,

    /// Original class name as it appears in source.
    #[serde(rename = "className")]
    pub class_name: String,

    /// DI lifetime: "request", "singleton", or "transient".
    pub lifetime: String,

    /// Constructor dependencies for DI resolution.
    pub dependencies: Vec<DependencyDescriptor>,

    /// HTTP handlers declared on this service.
    pub handlers: Vec<HandlerDescriptor>,

    /// Optional route group (from `@Group`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupDescriptor>,

    /// Middleware applied to all handlers in this service.
    #[serde(default)]
    pub middleware: Vec<HandlerMiddleware>,
}

/// A constructor dependency for DI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyDescriptor {
    /// Dependency kind: "service", "config", etc.
    #[serde(rename = "type")]
    pub dep_type: String,

    /// The token or class name to resolve.
    pub value: String,

    /// Constructor parameter position (0-indexed).
    pub position: u32,
}

/// A single `@Handler`-decorated method on a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerDescriptor {
    /// Method name on the service class.
    pub name: String,

    /// HTTP method (GET, POST, PUT, PATCH, DELETE).
    pub method: String,

    /// Route path, possibly with parameters (e.g., "/users/:id").
    pub path: String,

    /// HTTP status code to return on success.
    #[serde(default = "default_status")]
    pub status: u16,

    /// Whether JSON Schema validation is enabled for this handler.
    #[serde(default = "default_validate")]
    pub validate: bool,

    /// Whether the response is streamed chunk-by-chunk to the client
    /// (HTTP chunked transfer encoding) rather than buffered. Defaults to
    /// false so older manifests continue to deserialize unchanged.
    #[serde(default)]
    pub streaming: bool,

    /// Parameter extraction descriptors.
    pub extract: Vec<ExtractDescriptor>,

    /// Optional response schema for documentation / future validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,

    /// Middleware applied to this handler specifically.
    #[serde(default)]
    pub middleware: Vec<HandlerMiddleware>,
}

fn default_status() -> u16 {
    200
}

fn default_validate() -> bool {
    true
}

/// Describes how a single handler parameter is extracted from the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractDescriptor {
    /// Field name in the handler's input object — the key the user wrote in
    /// `extract: { id: Param("id"), body: Body() }`. Defaulted to an empty
    /// string for backward compatibility with manifests emitted before the
    /// record-shaped extract migration.
    #[serde(default)]
    pub field: String,

    /// Source of the value: "body", "query", "param", "header".
    pub source: String,

    /// Name of the query param / path param / header.
    /// `None` for body parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// JSON Schema for this parameter (inline or `$ref`).
    /// Present for body parameters; typically absent for scalar extracts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

// ──────────────────────────────────────────────
// Middleware & groups
// ──────────────────────────────────────────────

/// A single entry in a handler's or service's `middleware` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerMiddleware {
    /// "native" for gateway-executed (e.g. cors), "user" for worker-executed.
    pub kind: String,

    /// Middleware name / class name.
    pub name: String,

    /// Configuration blob for native middleware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// A `@Middleware`-decorated class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareDescriptor {
    /// Middleware name.
    pub name: String,

    /// Original class name.
    #[serde(rename = "className")]
    pub class_name: String,

    /// Whether this middleware applies globally.
    #[serde(default)]
    pub global: bool,

    /// Execution order (lower runs first).
    #[serde(default)]
    pub order: i32,

    /// "user" for app-defined, "native" for framework-provided.
    #[serde(default = "default_kind")]
    pub kind: String,

    /// Native middleware configuration (e.g., CorsConfig for kind = "native").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

fn default_kind() -> String {
    "user".to_string()
}

/// A `@Group`-decorated route group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDescriptor {
    /// URL prefix applied to all handlers in the group.
    pub prefix: String,

    /// Middleware applied to this group.
    #[serde(default)]
    pub middleware: Vec<HandlerMiddleware>,
}

// ──────────────────────────────────────────────
// Loading & validation
// ──────────────────────────────────────────────

impl Manifest {
    /// Load a manifest from a JSON file on disk.
    ///
    /// Reads the file, deserializes into typed structs, then runs
    /// all validation checks. Returns an error with an actionable
    /// message if anything is wrong.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ManifestError::Read {
            path: path.display().to_string(),
            source: e,
        })?;

        Self::from_json(&contents)
    }

    /// Parse and validate a manifest from a JSON string.
    ///
    /// Useful for testing without touching the filesystem.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_str(json)?;

        if manifest.version != MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: manifest.version,
                expected: MANIFEST_VERSION.to_string(),
            });
        }

        manifest.validate()?;
        Ok(manifest)
    }

    /// Run all internal consistency checks.
    fn validate(&self) -> Result<(), ManifestError> {
        self.check_duplicate_routes()?;
        self.check_schema_refs()?;
        self.check_extract_fields()?;
        Ok(())
    }

    /// Ensure no two handlers resolve to the same (method, full_path).
    fn check_duplicate_routes(&self) -> Result<(), ManifestError> {
        let mut seen = std::collections::HashSet::new();

        for service in &self.services {
            let prefix = service
                .group
                .as_ref()
                .map(|g| g.prefix.as_str())
                .unwrap_or("");

            for handler in &service.handlers {
                let full_path = format!("{}{}", prefix, handler.path);
                let route_key = format!("{} {}", handler.method.to_uppercase(), full_path);

                if !seen.insert(route_key.clone()) {
                    return Err(ManifestError::DuplicateRoute {
                        method: handler.method.clone(),
                        path: full_path,
                    });
                }
            }
        }

        Ok(())
    }

    /// Verify that every `$ref` in handler schemas resolves to an
    /// entry in `self.schemas`.
    fn check_schema_refs(&self) -> Result<(), ManifestError> {
        for service in &self.services {
            for handler in &service.handlers {
                for extract in &handler.extract {
                    if let Some(schema) = &extract.schema {
                        self.walk_refs(schema)?;
                    }
                }
                if let Some(response) = &handler.response {
                    self.walk_refs(response)?;
                }
            }
        }

        Ok(())
    }

    /// Recursively walk a JSON Schema value looking for `$ref` keys
    /// and verifying they resolve against `self.schemas`.
    fn walk_refs(&self, value: &serde_json::Value) -> Result<(), ManifestError> {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(ref_path)) = map.get("$ref") {
                    let name = ref_path
                        .strip_prefix("#/schemas/")
                        .unwrap_or(ref_path.as_str());
                    if !self.schemas.contains_key(name) {
                        return Err(ManifestError::UnresolvedRef(ref_path.clone()));
                    }
                }

                for v in map.values() {
                    self.walk_refs(v)?;
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr {
                    self.walk_refs(v)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Verify each handler's extract field names are unique. With the
    /// record-shaped extract (`{ id: ..., body: ... }`) emitted by the TS
    /// assembler, duplicates would silently shadow earlier entries — flag
    /// them at load time instead.
    fn check_extract_fields(&self) -> Result<(), ManifestError> {
        for service in &self.services {
            for handler in &service.handlers {
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for entry in &handler.extract {
                    // Pre-migration manifests emit an empty `field` (defaulted
                    // by serde); skip the uniqueness check in that case.
                    if entry.field.is_empty() {
                        continue;
                    }
                    if !seen.insert(entry.field.as_str()) {
                        return Err(ManifestError::DuplicateExtractField {
                            service: service.name.clone(),
                            handler: handler.name.clone(),
                            details: format!("duplicate field '{}'", entry.field),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Iterate all routes as `(http_method, full_path, service, handler)` tuples.
    ///
    /// Resolves group prefixes. Used by the dynamic router (E2-S8)
    /// to register axum routes at startup.
    #[allow(dead_code)]
    pub fn routes(&self) -> Vec<RouteEntry<'_>> {
        let mut routes = Vec::new();

        for service in &self.services {
            let prefix = service
                .group
                .as_ref()
                .map(|g| g.prefix.as_str())
                .unwrap_or("");

            for handler in &service.handlers {
                let full_path = format!("{}{}", prefix, handler.path);
                routes.push(RouteEntry {
                    method: &handler.method,
                    path: full_path,
                    service,
                    handler,
                });
            }
        }

        routes
    }

    /// Resolve a `$ref` string like `"#/schemas/CreateUserRequest"`
    /// to the actual schema value.
    pub fn resolve_schema(&self, ref_path: &str) -> Option<&serde_json::Value> {
        let name = ref_path.strip_prefix("#/schemas/").unwrap_or(ref_path);
        self.schemas.get(name)
    }
}

/// A resolved route with references back into the manifest.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RouteEntry<'a> {
    pub method: &'a str,
    pub path: String,
    pub service: &'a ServiceDescriptor,
    pub handler: &'a HandlerDescriptor,
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a minimal valid manifest JSON string.
    fn minimal_manifest() -> String {
        serde_json::json!({
            "version": "1",
            "hash": "abc123",
            "emitted_at": "2026-04-27T00:00:00Z",
            "services": [],
            "middleware": [],
            "schemas": {}
        })
        .to_string()
    }

    /// Helper: build a manifest with one service and one handler.
    fn single_handler_manifest(
        method: &str,
        path: &str,
        extract: serde_json::Value,
    ) -> String {
        serde_json::json!({
            "version": "1",
            "hash": "abc123",
            "emitted_at": "2026-04-27T00:00:00Z",
            "services": [{
                "name": "testService",
                "className": "TestService",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [{
                    "name": "handle",
                    "method": method,
                    "path": path,
                    "status": 200,
                    "validate": true,
                    "extract": extract
                }]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string()
    }

    // ── Basic loading ───────────────────────────

    #[test]
    fn loads_minimal_manifest() {
        let manifest = Manifest::from_json(&minimal_manifest()).unwrap();
        assert_eq!(manifest.version, "1");
        assert!(manifest.services.is_empty());
        assert!(manifest.schemas.is_empty());
    }

    #[test]
    fn rejects_wrong_version() {
        let json = serde_json::json!({
            "version": "99",
            "hash": "",
            "emitted_at": "",
            "services": [],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::UnsupportedVersion { .. }),
            "expected UnsupportedVersion, got: {err}"
        );
    }

    #[test]
    fn rejects_invalid_json() {
        let err = Manifest::from_json("{ not valid json }").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    // ── Service deserialization ──────────────────

    #[test]
    fn deserializes_full_service() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "def456",
            "emitted_at": "2026-04-27T00:00:00Z",
            "services": [{
                "name": "userService",
                "className": "UserService",
                "lifetime": "request",
                "dependencies": [
                    { "type": "service", "value": "DatabaseService", "position": 0 }
                ],
                "handlers": [
                    {
                        "name": "createUser",
                        "method": "POST",
                        "path": "/users",
                        "status": 201,
                        "validate": true,
                        "extract": [
                            {
                                "source": "body",
                                "position": 0,
                                "schema": { "$ref": "#/schemas/CreateUserRequest" }
                            }
                        ],
                        "response": { "$ref": "#/schemas/UserResponse" }
                    },
                    {
                        "name": "getUser",
                        "method": "GET",
                        "path": "/users/:id",
                        "status": 200,
                        "validate": false,
                        "extract": [
                            { "source": "param", "name": "id", "position": 0 }
                        ]
                    }
                ],
                "group": {
                    "prefix": "/api",
                    "middleware": [{ "kind": "user", "name": "AuthMiddleware" }]
                }
            }],
            "middleware": [{
                "name": "auth",
                "className": "AuthMiddleware",
                "global": false,
                "order": 10,
                "kind": "user"
            }],
            "schemas": {
                "CreateUserRequest": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "email": { "type": "string" }
                    },
                    "required": ["name", "email"]
                },
                "UserResponse": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "email": { "type": "string" }
                    },
                    "required": ["id", "name", "email"]
                }
            }
        })
        .to_string();

        let manifest = Manifest::from_json(&json).unwrap();

        // Service
        assert_eq!(manifest.services.len(), 1);
        let svc = &manifest.services[0];
        assert_eq!(svc.name, "userService");
        assert_eq!(svc.class_name, "UserService");
        assert_eq!(svc.lifetime, "request");
        assert_eq!(svc.dependencies.len(), 1);
        assert_eq!(svc.dependencies[0].dep_type, "service");
        assert_eq!(svc.dependencies[0].value, "DatabaseService");

        // Handlers
        assert_eq!(svc.handlers.len(), 2);
        assert_eq!(svc.handlers[0].name, "createUser");
        assert_eq!(svc.handlers[0].method, "POST");
        assert_eq!(svc.handlers[0].status, 201);
        assert!(svc.handlers[0].validate);
        assert_eq!(svc.handlers[1].name, "getUser");
        assert!(!svc.handlers[1].validate);

        // Group
        let group = svc.group.as_ref().unwrap();
        assert_eq!(group.prefix, "/api");
        assert_eq!(group.middleware.len(), 1);
        assert_eq!(group.middleware[0].kind, "user");
        assert_eq!(group.middleware[0].name, "AuthMiddleware");

        // Middleware
        assert_eq!(manifest.middleware.len(), 1);
        assert_eq!(manifest.middleware[0].order, 10);

        // Schemas
        assert_eq!(manifest.schemas.len(), 2);
        assert!(manifest.schemas.contains_key("CreateUserRequest"));
        assert!(manifest.schemas.contains_key("UserResponse"));
    }

    #[test]
    fn default_status_and_validate() {
        // Omit status and validate — should default to 200 and true
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "singleton",
                "dependencies": [],
                "handlers": [{
                    "name": "h",
                    "method": "GET",
                    "path": "/health",
                    "extract": []
                }]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let manifest = Manifest::from_json(&json).unwrap();
        let handler = &manifest.services[0].handlers[0];
        assert_eq!(handler.status, 200);
        assert!(handler.validate);
    }

    // ── Duplicate route detection ───────────────

    #[test]
    fn detects_duplicate_routes_same_service() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [
                    { "name": "h1", "method": "GET", "path": "/items", "status": 200, "validate": true, "extract": [] },
                    { "name": "h2", "method": "GET", "path": "/items", "status": 200, "validate": true, "extract": [] }
                ]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateRoute { .. }));
    }

    #[test]
    fn detects_duplicate_routes_across_services() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [
                {
                    "name": "a",
                    "className": "A",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [
                        { "name": "h", "method": "POST", "path": "/submit", "status": 200, "validate": true, "extract": [] }
                    ]
                },
                {
                    "name": "b",
                    "className": "B",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [
                        { "name": "h", "method": "POST", "path": "/submit", "status": 201, "validate": true, "extract": [] }
                    ]
                }
            ],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateRoute { .. }));
    }

    #[test]
    fn allows_same_path_different_methods() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [
                    { "name": "get", "method": "GET", "path": "/items", "status": 200, "validate": true, "extract": [] },
                    { "name": "post", "method": "POST", "path": "/items", "status": 201, "validate": true, "extract": [] }
                ]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        // Should succeed — same path, different methods is valid REST.
        Manifest::from_json(&json).unwrap();
    }

    #[test]
    fn detects_duplicates_with_group_prefix() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [
                {
                    "name": "a",
                    "className": "A",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [
                        { "name": "h", "method": "GET", "path": "/api/users", "status": 200, "validate": true, "extract": [] }
                    ]
                },
                {
                    "name": "b",
                    "className": "B",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [
                        { "name": "h", "method": "GET", "path": "/users", "status": 200, "validate": true, "extract": [] }
                    ],
                    "group": { "prefix": "/api", "middleware": [] }
                }
            ],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateRoute { .. }));
    }

    // ── Schema $ref resolution ──────────────────

    #[test]
    fn detects_unresolved_ref_in_extract() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [{
                    "name": "h",
                    "method": "POST",
                    "path": "/",
                    "status": 200,
                    "validate": true,
                    "extract": [
                        { "source": "body", "position": 0, "schema": { "$ref": "#/schemas/DoesNotExist" } }
                    ]
                }]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::UnresolvedRef(_)));
    }

    #[test]
    fn detects_unresolved_ref_in_response() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [{
                    "name": "h",
                    "method": "GET",
                    "path": "/",
                    "status": 200,
                    "validate": true,
                    "extract": [],
                    "response": { "$ref": "#/schemas/Phantom" }
                }]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::UnresolvedRef(_)));
    }

    #[test]
    fn detects_unresolved_ref_nested_in_properties() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [{
                    "name": "h",
                    "method": "POST",
                    "path": "/",
                    "status": 200,
                    "validate": true,
                    "extract": [{
                        "source": "body",
                        "position": 0,
                        "schema": {
                            "type": "object",
                            "properties": {
                                "nested": { "$ref": "#/schemas/Missing" }
                            }
                        }
                    }]
                }]
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::UnresolvedRef(_)));
    }

    #[test]
    fn accepts_valid_ref() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "s",
                "className": "S",
                "lifetime": "request",
                "dependencies": [],
                "handlers": [{
                    "name": "h",
                    "method": "POST",
                    "path": "/",
                    "status": 200,
                    "validate": true,
                    "extract": [{
                        "source": "body",
                        "position": 0,
                        "schema": { "$ref": "#/schemas/MyType" }
                    }]
                }]
            }],
            "middleware": [],
            "schemas": {
                "MyType": {
                    "type": "object",
                    "properties": { "x": { "type": "number" } },
                    "required": ["x"]
                }
            }
        })
        .to_string();

        Manifest::from_json(&json).unwrap();
    }

    // ── Extract field uniqueness ────────────────

    #[test]
    fn detects_duplicate_extract_fields() {
        let json = single_handler_manifest(
            "POST",
            "/test",
            serde_json::json!([
                { "field": "x", "source": "query", "name": "page" },
                { "field": "x", "source": "query", "name": "limit" }
            ]),
        );

        let err = Manifest::from_json(&json).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateExtractField { .. }));
    }

    #[test]
    fn accepts_unique_extract_fields() {
        let json = single_handler_manifest(
            "POST",
            "/test",
            serde_json::json!([
                { "field": "body",  "source": "body" },
                { "field": "page",  "source": "query",  "name": "page" },
                { "field": "trace", "source": "header", "name": "x-request-id" }
            ]),
        );

        Manifest::from_json(&json).unwrap();
    }

    #[test]
    fn accepts_empty_extract() {
        let json = single_handler_manifest("GET", "/health", serde_json::json!([]));
        Manifest::from_json(&json).unwrap();
    }

    // ── routes() helper ─────────────────────────

    #[test]
    fn routes_resolves_group_prefix() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [{
                "name": "admin",
                "className": "AdminService",
                "lifetime": "singleton",
                "dependencies": [],
                "handlers": [
                    { "name": "list", "method": "GET", "path": "/users", "status": 200, "validate": true, "extract": [] },
                    { "name": "create", "method": "POST", "path": "/users", "status": 201, "validate": true, "extract": [] }
                ],
                "group": { "prefix": "/api/admin", "middleware": [] }
            }],
            "middleware": [],
            "schemas": {}
        })
        .to_string();

        let manifest = Manifest::from_json(&json).unwrap();
        let routes = manifest.routes();

        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/api/admin/users");
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[1].path, "/api/admin/users");
        assert_eq!(routes[1].method, "POST");
    }

    #[test]
    fn routes_without_group_uses_bare_path() {
        let json = single_handler_manifest("GET", "/health", serde_json::json!([]));
        let manifest = Manifest::from_json(&json).unwrap();
        let routes = manifest.routes();

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/health");
    }

    // ── resolve_schema() ────────────────────────

    #[test]
    fn resolve_schema_works() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [],
            "middleware": [],
            "schemas": {
                "Foo": { "type": "object" }
            }
        })
        .to_string();

        let manifest = Manifest::from_json(&json).unwrap();

        assert!(manifest.resolve_schema("#/schemas/Foo").is_some());
        assert!(manifest.resolve_schema("Foo").is_some());
        assert!(manifest.resolve_schema("#/schemas/Bar").is_none());
    }

    // ── File loading error ──────────────────────

    #[test]
    fn reports_missing_file() {
        let err = Manifest::from_file(Path::new("/nonexistent/sky-manifest.json")).unwrap_err();
        assert!(matches!(err, ManifestError::Read { .. }));
    }

    // ── Nullable type support ───────────────────

    #[test]
    fn nullable_type_in_schema() {
        let json = serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [],
            "middleware": [],
            "schemas": {
                "NullableField": {
                    "type": ["string", "null"],
                    "properties": {}
                }
            }
        })
        .to_string();

        let manifest = Manifest::from_json(&json).unwrap();
        let schema = &manifest.schemas["NullableField"];

        // Since schemas is HashMap<String, serde_json::Value>,
        // the type field is a JSON array.
        let type_val = schema.get("type").unwrap();
        assert!(type_val.is_array());
        let types = type_val.as_array().unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0], "string");
        assert_eq!(types[1], "null");
    }
}