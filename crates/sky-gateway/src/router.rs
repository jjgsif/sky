//! Manifest-driven dynamic router for the Sky gateway.
//!
//! Replaces the generic `InvokeHandler` approach with per-service,
//! per-handler gRPC calls. The gateway encodes typed request messages
//! dynamically using raw proto encoding based on the manifest's extract
//! descriptors, and all RPCs return `SkyResponse`.
//!
//! The gateway does NOT use generated client stubs. Instead, it builds
//! proto messages at runtime from the manifest's field layout and makes
//! raw unary gRPC calls. This keeps the router fully manifest-driven
//! without coupling to any specific set of generated types.
//!
//! # Request encoding
//!
//! Each handler's extract descriptors define the request message fields:
//! - Body() → `bytes body` (field number based on position in extracts)
//! - Param(name) → `string {name}` field
//! - Query(name) → `string {name}` field
//! - Header(name) → `string {name}` field
//!
//! Field numbers are assigned sequentially starting at 1, matching
//! the proto emitter's output.
//!
//! # Response decoding
//!
//! All RPCs return `SkyResponse`, which IS a known compiled type
//! (from sky_response.proto). The gateway decodes it and maps
//! status, body, headers, and cookies into the HTTP response.

use crate::manifest::Manifest;
use crate::validation::{HandlerKey, SchemaRegistry, ValidationErrorResponse};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::Serialize;
use sky_runtime::RequestId;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

// Import the compiled SkyResponse and SetCookie types.
use sky_proto::v1::{SetCookie, SkyResponse};

// ──────────────────────────────────────────────
// Shared state
// ──────────────────────────────────────────────

/// Application state shared across all route handlers.
#[derive(Clone)]
pub struct RouterState {
    /// The loaded manifest.
    pub manifest: Arc<Manifest>,

    /// Compiled JSON Schema validators.
    pub schema_registry: Arc<SchemaRegistry>,

    /// gRPC channel to the worker.
    pub channel: Channel,
}

/// Per-route metadata attached as an axum `Extension`.
#[derive(Clone, Debug)]
struct RouteInfo {
    /// Key for SchemaRegistry lookup.
    key: HandlerKey,

    /// gRPC method path: "/sky.v1.{ClassName}/{RpcName}"
    grpc_path: String,

    /// The declared success status code (used as fallback if
    /// the handler doesn't set one explicitly via Response).
    default_status: u16,

    /// Extract descriptors with field numbers for proto encoding.
    fields: Arc<Vec<ProtoField>>,
}

/// A field in the dynamically-built proto request message.
#[derive(Clone, Debug)]
struct ProtoField {
    /// Proto field number (1-indexed, sequential).
    field_number: u32,

    /// What to extract from the HTTP request.
    source: FieldSource,

    /// Name of the param/query/header (None for body).
    name: Option<String>,
}

#[derive(Clone, Debug)]
enum FieldSource {
    Body,
    Param,
    Query,
    Header,
}

// ──────────────────────────────────────────────
// Router construction
// ──────────────────────────────────────────────

/// Build an axum `Router` from the manifest.
pub fn build_manifest_router(state: RouterState) -> Router {
    let manifest = &state.manifest;
    let mut router = Router::new();
    let mut route_count = 0;

    for service in &manifest.services {
        let prefix = service
            .group
            .as_ref()
            .map(|g| g.prefix.as_str())
            .unwrap_or("");

        for handler in &service.handlers {
            let full_path = format!("{}{}", prefix, handler.path);

            // Build the gRPC method path.
            let rpc_name = capitalize_first(&handler.name);
            let grpc_path = format!("/sky.v1.{}/{}", service.class_name, rpc_name);

            // Build proto field descriptors from extracts.
            let fields = build_proto_fields(&handler.extract);

            let route_info = RouteInfo {
                key: HandlerKey {
                    service: service.name.clone(),
                    handler: handler.name.clone(),
                },
                grpc_path: grpc_path.clone(),
                default_status: handler.status,
                fields: Arc::new(fields),
            };

            let method_router = match handler.method.to_uppercase().as_str() {
                "GET" => get(generic_handler),
                "POST" => post(generic_handler),
                "PUT" => put(generic_handler),
                "PATCH" => patch(generic_handler),
                "DELETE" => delete(generic_handler),
                other => {
                    warn!(
                        method = other,
                        path = %full_path,
                        "unsupported HTTP method in manifest, skipping"
                    );
                    continue;
                }
            };

            let layered = method_router.layer(Extension(route_info));
            router = router.route(&full_path, layered);
            route_count += 1;

            debug!(
                method = %handler.method,
                path = %full_path,
                service = %service.name,
                handler = %handler.name,
                grpc = %grpc_path.clone(),
                "registered route"
            );
        }
    }

    info!(count = route_count, "manifest routes registered");

    router.with_state(state)
}

/// Build proto field descriptors from extract descriptors.
fn build_proto_fields(extracts: &[crate::manifest::ExtractDescriptor]) -> Vec<ProtoField> {
    extracts
        .iter()
        .enumerate()
        .map(|(i, e)| ProtoField {
            field_number: (i + 1) as u32,
            source: match e.source.as_str() {
                "body" => FieldSource::Body,
                "param" => FieldSource::Param,
                "query" => FieldSource::Query,
                "header" => FieldSource::Header,
                other => {
                    warn!(source = other, "unknown extract source, treating as body");
                    FieldSource::Body
                }
            },
            name: e.name.clone(),
        })
        .collect()
}

// ──────────────────────────────────────────────
// Generic request handler
// ──────────────────────────────────────────────

/// The single axum handler function that serves all manifest-driven routes.
async fn generic_handler(
    State(state): State<RouterState>,
    Extension(route): Extension<RouteInfo>,
    method: Method,
    Path(path_params): Path<HashMap<String, String>>,
    Query(query_params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = RequestId::new();

    debug!(
        request_id = %request_id,
        grpc_path = %route.grpc_path,
        method = %method,
        "handling request"
    );

    // ── Parse and validate body ─────────────────
    let has_body_field = route
        .fields
        .iter()
        .any(|f| matches!(f.source, FieldSource::Body));

    let body_bytes = if has_body_field && !body.is_empty() {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(value) => {
                if let Err(validation_err) = state.schema_registry.validate(&route.key, &value) {
                    return make_validation_error_response(validation_err);
                }
                body.to_vec()
            }
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    &format!("Failed to parse request body as JSON: {e}"),
                );
            }
        }
    } else if has_body_field && body.is_empty() {
        let empty = serde_json::Value::Object(serde_json::Map::new());
        if let Err(validation_err) = state.schema_registry.validate(&route.key, &empty) {
            return make_validation_error_response(validation_err);
        }
        b"{}".to_vec()
    } else {
        Vec::new()
    };

    // ── Encode proto request message ────────────
    let request_bytes = encode_request_message(
        &route.fields,
        &body_bytes,
        &path_params,
        &query_params,
        &headers,
    );

    // ── Make gRPC call ──────────────────────────
    let mut grpc_client = tonic::client::Grpc::new(state.channel.clone());

    let grpc_path: tonic::codegen::http::uri::PathAndQuery =
        match route.grpc_path.parse() {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, path = %route.grpc_path, "invalid gRPC path");
                return make_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Invalid gRPC path configuration",
                );
            }
        };

    if let Err(e) = grpc_client.ready().await {
        error!(error = %e, "gRPC channel not ready");
        return make_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "worker_unreachable",
            "Worker is not available",
        );
    }

    let codec = tonic::codec::ProstCodec::default();
    let request = tonic::Request::new(RawMessage(request_bytes));

    let result: Result<tonic::Response<SkyResponse>, tonic::Status> =
        grpc_client.unary(request, grpc_path, codec).await;

    match result {
        Ok(response) => {
            let sky_response = response.into_inner();
            build_http_response(sky_response, route.default_status, &request_id)
        }
        Err(grpc_err) => {
            error!(
                request_id = %request_id,
                grpc_path = %route.grpc_path,
                error = %grpc_err,
                "worker invocation failed"
            );

            let (status, code) = map_grpc_error(grpc_err.code());
            make_error_response(status, code, grpc_err.message())
        }
    }
}

// ──────────────────────────────────────────────
// Proto encoding
// ──────────────────────────────────────────────

/// A raw proto message wrapper for sending dynamically-encoded bytes
/// through tonic's unary call.
#[derive(Clone, Debug)]
struct RawMessage(Vec<u8>);

impl prost::Message for RawMessage {
    fn encode_raw(&self, buf: &mut impl prost::bytes::BufMut) {
        buf.put_slice(&self.0);
    }

    fn merge_field(
        &mut self,
        _tag: u32,
        _wire_type: prost::encoding::WireType,
        _buf: &mut impl prost::bytes::Buf,
        _ctx: prost::encoding::DecodeContext,
    ) -> Result<(), prost::DecodeError> {
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        self.0.len()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// Encode a proto request message from extract field definitions.
fn encode_request_message(
    fields: &[ProtoField],
    body_bytes: &[u8],
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Vec<u8> {
    let mut buf = Vec::new();

    for field in fields {
        match &field.source {
            FieldSource::Body => {
                if !body_bytes.is_empty() {
                    encode_bytes_field(&mut buf, field.field_number, body_bytes);
                }
            }
            FieldSource::Param => {
                if let Some(name) = &field.name {
                    if let Some(value) = path_params.get(name) {
                        encode_string_field(&mut buf, field.field_number, value);
                    }
                }
            }
            FieldSource::Query => {
                if let Some(name) = &field.name {
                    if let Some(value) = query_params.get(name) {
                        encode_string_field(&mut buf, field.field_number, value);
                    }
                }
            }
            FieldSource::Header => {
                if let Some(name) = &field.name {
                    if let Some(value) = headers.get(name.as_str()) {
                        if let Ok(v) = value.to_str() {
                            encode_string_field(&mut buf, field.field_number, v);
                        }
                    }
                }
            }
        }
    }

    buf
}

/// Encode a proto varint.
fn encode_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Encode a proto tag (field number + wire type).
fn encode_tag(buf: &mut Vec<u8>, field_number: u32, wire_type: u8) {
    encode_varint(buf, ((field_number as u64) << 3) | wire_type as u64);
}

/// Encode a length-delimited bytes field.
fn encode_bytes_field(buf: &mut Vec<u8>, field_number: u32, data: &[u8]) {
    encode_tag(buf, field_number, 2);
    encode_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Encode a length-delimited string field.
fn encode_string_field(buf: &mut Vec<u8>, field_number: u32, value: &str) {
    encode_bytes_field(buf, field_number, value.as_bytes());
}

// ──────────────────────────────────────────────
// Response building
// ──────────────────────────────────────────────

/// Convert a `SkyResponse` into an HTTP response.
fn build_http_response(
    sky_response: SkyResponse,
    default_status: u16,
    request_id: &RequestId,
) -> Response {
    let status_code = if sky_response.status > 0 {
        sky_response.status as u16
    } else {
        default_status
    };

    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);

    let mut response = if sky_response.body.is_empty() {
        (status, "").into_response()
    } else {
        let mut resp = (status, sky_response.body).into_response();
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        resp
    };

    // Merge response headers.
    for (key, value) in &sky_response.headers {
        if let (Ok(name), Ok(val)) = (
            axum::http::header::HeaderName::from_bytes(key.as_bytes()),
            axum::http::header::HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(name, val);
        }
    }

    // Set cookies via Set-Cookie headers.
    for cookie in &sky_response.cookies {
        let cookie_str = format_set_cookie(cookie);
        if let Ok(val) = axum::http::header::HeaderValue::from_str(&cookie_str) {
            response
                .headers_mut()
                .append(axum::http::header::SET_COOKIE, val);
        }
    }

    // Always include request ID.
    if let Ok(val) = axum::http::header::HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert("x-request-id", val);
    }

    response
}

/// Format a `SetCookie` message into a `Set-Cookie` header value.
fn format_set_cookie(cookie: &SetCookie) -> String {
    let mut parts = vec![format!("{}={}", cookie.name, cookie.value)];

    if let Some(max_age) = cookie.max_age {
        parts.push(format!("Max-Age={}", max_age));
    }
    if let Some(ref path) = cookie.path {
        parts.push(format!("Path={}", path));
    }
    if let Some(ref domain) = cookie.domain {
        parts.push(format!("Domain={}", domain));
    }
    if cookie.http_only {
        parts.push("HttpOnly".to_string());
    }
    if cookie.secure {
        parts.push("Secure".to_string());
    }
    if let Some(ref same_site) = cookie.same_site {
        parts.push(format!("SameSite={}", same_site));
    }

    parts.join("; ")
}

/// Map gRPC status codes to HTTP status codes.
fn map_grpc_error(code: tonic::Code) -> (StatusCode, &'static str) {
    match code {
        tonic::Code::NotFound => (StatusCode::NOT_FOUND, "handler_not_found"),
        tonic::Code::InvalidArgument => (StatusCode::BAD_REQUEST, "invalid_argument"),
        tonic::Code::DeadlineExceeded => (StatusCode::GATEWAY_TIMEOUT, "worker_timeout"),
        tonic::Code::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "worker_unreachable"),
        tonic::Code::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "worker_internal_error"),
        tonic::Code::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
        tonic::Code::PermissionDenied => (StatusCode::FORBIDDEN, "forbidden"),
        _ => (StatusCode::BAD_GATEWAY, "worker_error"),
    }
}

// ──────────────────────────────────────────────
// Response helpers
// ──────────────────────────────────────────────

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

fn make_error_response(status: StatusCode, code: &'static str, message: &str) -> Response {
    let body = ErrorBody {
        code,
        message: message.to_string(),
    };
    (status, Json(body)).into_response()
}

fn make_validation_error_response(err: ValidationErrorResponse) -> Response {
    (StatusCode::BAD_REQUEST, Json(err)).into_response()
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_router_state(manifest_json: &str) -> RouterState {
        let manifest = Manifest::from_json(manifest_json).unwrap();
        let schema_registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let channel = Channel::from_static("http://[::1]:50051").connect_lazy();

        RouterState {
            manifest: Arc::new(manifest),
            schema_registry: Arc::new(schema_registry),
            channel,
        }
    }

    fn test_manifest() -> String {
        serde_json::json!({
            "version": "1",
            "hash": "",
            "emitted_at": "",
            "services": [
                {
                    "name": "userService",
                    "className": "UserService",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [
                        {
                            "name": "createUser",
                            "method": "POST",
                            "path": "/users",
                            "status": 201,
                            "validate": true,
                            "extract": [{
                                "source": "body",
                                "position": 0,
                                "schema": { "$ref": "#/schemas/CreateUser" }
                            }]
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
                        },
                        {
                            "name": "listUsers",
                            "method": "GET",
                            "path": "/users",
                            "status": 200,
                            "validate": false,
                            "extract": [
                                { "source": "query", "name": "page", "position": 0 },
                                { "source": "query", "name": "limit", "position": 1 }
                            ]
                        },
                        {
                            "name": "deleteUser",
                            "method": "DELETE",
                            "path": "/users/:id",
                            "status": 204,
                            "validate": false,
                            "extract": [
                                { "source": "param", "name": "id", "position": 0 }
                            ]
                        }
                    ]
                },
                {
                    "name": "healthService",
                    "className": "HealthService",
                    "lifetime": "singleton",
                    "dependencies": [],
                    "handlers": [{
                        "name": "check",
                        "method": "GET",
                        "path": "/health",
                        "status": 200,
                        "validate": false,
                        "extract": []
                    }]
                },
                {
                    "name": "adminService",
                    "className": "AdminService",
                    "lifetime": "request",
                    "dependencies": [],
                    "handlers": [{
                        "name": "listAdminUsers",
                        "method": "GET",
                        "path": "/users",
                        "status": 200,
                        "validate": false,
                        "extract": [
                            { "source": "header", "name": "x-admin-token", "position": 0 }
                        ]
                    }],
                    "group": { "prefix": "/api/admin", "middleware": [] }
                }
            ],
            "middleware": [],
            "schemas": {
                "CreateUser": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "email": { "type": "string" }
                    },
                    "required": ["name", "email"]
                }
            }
        })
        .to_string()
    }

    // ── Route matching ──────────────────────────

    #[tokio::test]
    async fn undefined_route_returns_404() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_method_returns_405() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ── Body validation ─────────────────────────

    #[tokio::test]
    async fn invalid_json_body_returns_400() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("content-type", "application/json")
            .body(Body::from("not valid json"))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "invalid_json");
    }

    #[tokio::test]
    async fn schema_validation_failure_returns_400() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name": "Alice"}"#))
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "validation_failed");
        assert!(json["errors"].is_array());
    }

    #[tokio::test]
    async fn empty_body_on_body_handler_validates_schema() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/users")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── Group prefix ────────────────────────────

    #[tokio::test]
    async fn group_prefix_route_is_reachable() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        let req = Request::builder()
            .uri("/api/admin/users")
            .header("x-admin-token", "test-token")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Proto encoding ──────────────────────────

    #[test]
    fn encodes_empty_message() {
        let fields = vec![];
        let buf = encode_request_message(
            &fields,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HeaderMap::new(),
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn encodes_body_only() {
        let fields = vec![ProtoField {
            field_number: 1,
            source: FieldSource::Body,
            name: None,
        }];

        let body = b"{\"name\":\"Alice\"}";
        let buf = encode_request_message(
            &fields,
            body,
            &HashMap::new(),
            &HashMap::new(),
            &HeaderMap::new(),
        );

        assert!(!buf.is_empty());
        // Tag for field 1, wire type 2: (1 << 3) | 2 = 0x0A
        assert_eq!(buf[0], 0x0A);
    }

    #[test]
    fn encodes_string_param() {
        let fields = vec![ProtoField {
            field_number: 1,
            source: FieldSource::Param,
            name: Some("id".to_string()),
        }];

        let mut params = HashMap::new();
        params.insert("id".to_string(), "abc-123".to_string());

        let buf = encode_request_message(
            &fields,
            &[],
            &params,
            &HashMap::new(),
            &HeaderMap::new(),
        );

        assert!(!buf.is_empty());
        assert_eq!(buf[0], 0x0A);
        let decoded = String::from_utf8_lossy(&buf);
        assert!(decoded.contains("abc-123"));
    }

    #[test]
    fn encodes_multiple_fields() {
        let fields = vec![
            ProtoField {
                field_number: 1,
                source: FieldSource::Param,
                name: Some("id".to_string()),
            },
            ProtoField {
                field_number: 2,
                source: FieldSource::Body,
                name: None,
            },
        ];

        let mut params = HashMap::new();
        params.insert("id".to_string(), "xyz".to_string());
        let body = b"{\"x\":1}";

        let buf = encode_request_message(
            &fields,
            body,
            &params,
            &HashMap::new(),
            &HeaderMap::new(),
        );

        assert!(buf.len() > 10);
        // Field 1 tag: 0x0A, Field 2 tag: (2 << 3) | 2 = 0x12
        assert_eq!(buf[0], 0x0A);
        assert!(buf.contains(&0x12));
    }

    #[test]
    fn skips_missing_param() {
        let fields = vec![ProtoField {
            field_number: 1,
            source: FieldSource::Param,
            name: Some("id".to_string()),
        }];

        let buf = encode_request_message(
            &fields,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HeaderMap::new(),
        );

        assert!(buf.is_empty());
    }

    #[test]
    fn encodes_query_params() {
        let fields = vec![
            ProtoField {
                field_number: 1,
                source: FieldSource::Query,
                name: Some("page".to_string()),
            },
            ProtoField {
                field_number: 2,
                source: FieldSource::Query,
                name: Some("limit".to_string()),
            },
        ];

        let mut query = HashMap::new();
        query.insert("page".to_string(), "1".to_string());
        query.insert("limit".to_string(), "20".to_string());

        let buf = encode_request_message(
            &fields,
            &[],
            &HashMap::new(),
            &query,
            &HeaderMap::new(),
        );

        let decoded = String::from_utf8_lossy(&buf);
        assert!(decoded.contains("1"));
        assert!(decoded.contains("20"));
    }

    #[test]
    fn encodes_header_field() {
        let fields = vec![ProtoField {
            field_number: 1,
            source: FieldSource::Header,
            name: Some("x-admin-token".to_string()),
        }];

        let mut headers = HeaderMap::new();
        headers.insert("x-admin-token", "secret".parse().unwrap());

        let buf = encode_request_message(
            &fields,
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &headers,
        );

        let decoded = String::from_utf8_lossy(&buf);
        assert!(decoded.contains("secret"));
    }

    // ── Cookie formatting ───────────────────────

    #[test]
    fn formats_simple_cookie() {
        let cookie = SetCookie {
            name: "session".to_string(),
            value: "abc123".to_string(),
            max_age: None,
            path: None,
            domain: None,
            http_only: false,
            secure: false,
            same_site: None,
        };

        assert_eq!(format_set_cookie(&cookie), "session=abc123");
    }

    #[test]
    fn formats_full_cookie() {
        let cookie = SetCookie {
            name: "session".to_string(),
            value: "abc123".to_string(),
            max_age: Some(3600),
            path: Some("/".to_string()),
            domain: Some("example.com".to_string()),
            http_only: true,
            secure: true,
            same_site: Some("Strict".to_string()),
        };

        let result = format_set_cookie(&cookie);
        assert!(result.contains("session=abc123"));
        assert!(result.contains("Max-Age=3600"));
        assert!(result.contains("Path=/"));
        assert!(result.contains("Domain=example.com"));
        assert!(result.contains("HttpOnly"));
        assert!(result.contains("Secure"));
        assert!(result.contains("SameSite=Strict"));
    }

    #[test]
    fn formats_clear_cookie() {
        let cookie = SetCookie {
            name: "old_session".to_string(),
            value: "".to_string(),
            max_age: Some(0),
            path: Some("/".to_string()),
            domain: None,
            http_only: false,
            secure: false,
            same_site: None,
        };

        let result = format_set_cookie(&cookie);
        assert!(result.contains("old_session="));
        assert!(result.contains("Max-Age=0"));
    }

    // ── Route counting ──────────────────────────

    #[tokio::test]
    async fn router_state_builds_successfully() {
        let state = test_router_state(&test_manifest());
        let routes = state.manifest.routes();
        assert_eq!(routes.len(), 6);
    }

    // ── Helper functions ────────────────────────

    #[test]
    fn capitalize_first_works() {
        assert_eq!(capitalize_first("createUser"), "CreateUser");
        assert_eq!(capitalize_first("check"), "Check");
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("A"), "A");
    }
}