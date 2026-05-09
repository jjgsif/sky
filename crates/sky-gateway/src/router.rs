//! Manifest-driven dynamic router for the Sky gateway.
//!
//! Reads the manifest at startup and registers one axum route per handler.
//! Each route is served by a single generic handler that:
//!
//!   1. Validates the request body against the handler's JSON Schema.
//!   2. Sends an INVOKE frame to the worker via the Sky framing protocol.
//!   3. Streams RESPONSE_HEAD / RESPONSE_CHUNK / RESPONSE_END frames back.
//!   4. Builds an HTTP response from the worker's status, headers, and body.
//!
//! No generated stubs. No Protobuf encoding. The entire request is forwarded
//! as-is (body, params, query, headers) and the worker's dispatcher handles
//! field extraction based on the manifest.

use crate::auth::{AuthOutcome, AuthValidator};
use crate::cors::{apply_cors_headers, CorsRegistry};
use crate::manifest::Manifest;
use crate::rate_limit::{RateLimitOutcome, RateLimiter};
use crate::validation::{HandlerKey, SchemaRegistry, ValidationErrorResponse};
use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Path, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, options, patch, post, put};
use axum::{Extension, Json, Router};
use serde::Serialize;
use sky_runtime::RequestId;
use sky_worker::{InboundFrame, InvokePayload, PendingRequest, WorkerPool, WorkerSocket};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, warn};
// ── Shared state ──────────────────────────────────────────────────────────────

/// Application state shared across all route handlers.
#[derive(Clone)]
pub struct RouterState {
    pub manifest: Arc<Manifest>,
    pub schema_registry: Arc<SchemaRegistry>,
    pub cors: Arc<CorsRegistry>,
    pub rate_limit: Arc<RateLimiter>,
    pub auth: Arc<AuthValidator>,
    /// Worker pool. `None` in validation-only tests that don't need a real worker.
    pub pool: Option<Arc<WorkerPool>>,
}

/// Per-route metadata attached as an axum `Extension`.
#[derive(Clone, Debug)]
struct RouteInfo {
    /// Key for SchemaRegistry lookup.
    key: HandlerKey,

    /// "ClassName.handlerName" used in the INVOKE frame's handler_id field.
    handler_id: String,

    /// Declared success status code — used when the worker sends status 0.
    default_status: u16,

    /// Whether this handler has a Body() extract (for validation skipping).
    has_body: bool,

    /// Whether this handler streams its response body to the client via
    /// HTTP chunked transfer encoding (true) or buffers it before
    /// responding (false). Drives the branch in `generic_handler`.
    streaming: bool,
}

// ── Router construction ───────────────────────────────────────────────────────

/// Build an axum `Router` from the manifest.
pub fn build_manifest_router(state: RouterState) -> Router {
    let manifest = &state.manifest;
    let mut router = Router::new();
    let mut route_count = 0;
    // Track which paths have had an OPTIONS route registered (one per path, not per handler).
    let mut options_registered: std::collections::HashSet<String> = std::collections::HashSet::new();

    for service in &manifest.services {
        let prefix = service
            .group
            .as_ref()
            .map(|g| g.prefix.as_str())
            .unwrap_or("");

        for handler in &service.handlers {
            let full_path = format!("{}{}", prefix, handler.path);
            let handler_id = format!("{}.{}", service.class_name, handler.name);
            let has_body = handler.extract.iter().any(|e| e.source == "body");

            let route_info = RouteInfo {
                key: HandlerKey {
                    service: service.name.clone(),
                    handler: handler.name.clone(),
                },
                handler_id: handler_id.clone(),
                default_status: handler.status,
                has_body,
                streaming: handler.streaming,
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
                        "unsupported HTTP method in manifest; skipping"
                    );
                    continue;
                }
            };

            let layered = method_router.layer(Extension(route_info));
            router = router.route(&full_path, layered);
            route_count += 1;

            // Register a single OPTIONS handler per path for CORS preflight.
            let has_cors = handler
                .middleware
                .iter()
                .any(|m| m.kind == "native" && m.name == "cors");
            if has_cors && options_registered.insert(full_path.clone()) {
                router = router.route(&full_path, options(cors_preflight_handler));
            }

            debug!(
                method = %handler.method,
                path = %full_path,
                service = %service.name,
                handler = %handler.name,
                handler_id = %handler_id,
                "registered route"
            );
        }
    }

    tracing::info!(count = route_count, "manifest routes registered");
    router.with_state(state)
}

// ── CORS preflight handler ────────────────────────────────────────────────────

async fn cors_preflight_handler(
    State(state): State<RouterState>,
    matched_path: MatchedPath,
    headers: HeaderMap,
) -> Response {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    state
        .cors
        .preflight(matched_path.as_str(), origin)
        .unwrap_or_else(|| StatusCode::NO_CONTENT.into_response())
}

// ── Generic request handler ───────────────────────────────────────────────────

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
    let origin_str: Option<String> = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    debug!(
        request_id = %request_id,
        handler_id = %route.handler_id,
        method = %method,
        "handling request"
    );

    // ── Body validation ──────────────────────────────────────────────────────

    let body_bytes: Vec<u8> = if route.has_body && !body.is_empty() {
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
    } else if route.has_body && body.is_empty() {
        let empty = serde_json::Value::Object(serde_json::Map::new());
        if let Err(validation_err) = state.schema_registry.validate(&route.key, &empty) {
            return make_validation_error_response(validation_err);
        }
        b"{}".to_vec()
    } else {
        Vec::new()
    };

    // ── Rate limiting ────────────────────────────────────────────────────────

    let peer_ip = resolve_peer_ip(&headers);
    if let RateLimitOutcome::Denied { retry_after_secs } =
        state.rate_limit.check_and_record(&route.handler_id, &headers, &peer_ip).await
    {
        return make_rate_limited_response(retry_after_secs);
    }

    // ── Authentication ───────────────────────────────────────────────────────

    let verified_claims = match state.auth.check(&route.handler_id, &headers) {
        AuthOutcome::Allowed { claims } => Some(claims),
        AuthOutcome::NotRequired => None,
        AuthOutcome::Denied { status, code, message } => {
            return make_error_response(status, code, &message);
        }
    };

    // ── Worker connection check ──────────────────────────────────────────────

    let connection: Arc<WorkerSocket> = match &state.pool {
        Some(pool) => pool.acquire(),
        None => {
            error!("no worker pool available");
            return make_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "worker_unavailable",
                "Worker is not connected",
            );
        }
    };

    // ── Build INVOKE payload ─────────────────────────────────────────────────

    let mut header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();

    if let Some(claims) = verified_claims {
        header_map.insert("x-sky-claims".to_string(), claims.to_string());
    }

    let payload = InvokePayload {
        handler_id: &route.handler_id,
        method: method.as_str(),
        path: &format!("/{}", path_params.values().cloned().collect::<Vec<_>>().join("/")),
        params: &path_params,
        query: &query_params,
        headers: header_map,
        body: &body_bytes,
    };

    // ── Send INVOKE, collect response frames ─────────────────────────────────

    let mut pending = match connection.invoke(&payload, "default").await {
        Ok(p) => p,
        Err(e) => {
            error!(
                request_id = %request_id,
                handler_id = %route.handler_id,
                error = %e,
                "INVOKE failed"
            );
            return make_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "worker_unreachable",
                &e.to_string(),
            );
        }
    };

    // First frame must be RESPONSE_HEAD (or ERROR). Both code paths share this.
    let (status, response_headers) = match pending.next_frame().await {
        Some(InboundFrame::Head { status, headers }) => (status, headers),
        Some(InboundFrame::Error { message, .. }) => {
            error!(
                request_id = %request_id,
                handler_id = %route.handler_id,
                error = %message,
                "worker returned ERROR frame"
            );
            return make_error_response(StatusCode::BAD_GATEWAY, "worker_error", &message);
        }
        Some(_) => {
            error!(request_id = %request_id, "unexpected first frame type");
            return make_error_response(
                StatusCode::BAD_GATEWAY,
                "protocol_error",
                "Unexpected first response frame",
            );
        }
        None => {
            error!(request_id = %request_id, "worker connection closed before HEAD");
            return make_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "worker_disconnected",
                "Worker disconnected during request",
            );
        }
    };

    let mut response = if route.streaming {
        build_streaming_response(
            status,
            response_headers,
            pending,
            route.default_status,
            &request_id,
            &route.handler_id,
        )
    } else {
        let body_chunks = drain_buffered(&mut pending, &request_id, &route.handler_id).await;
        build_http_response(
            status,
            response_headers,
            body_chunks,
            route.default_status,
            &request_id,
        )
    };

    if let (Some(origin), Some(policy)) =
        (origin_str.as_deref(), state.cors.get_by_handler(&route.handler_id))
    {
        apply_cors_headers(policy, origin, response.headers_mut());
    }

    response
}

// ── Body drainers ─────────────────────────────────────────────────────────────

/// Buffered path: collect every chunk until END/ERROR, return the full body.
async fn drain_buffered(
    pending: &mut PendingRequest,
    request_id: &RequestId,
    handler_id: &str,
) -> Vec<u8> {
    let mut body_chunks: Vec<u8> = Vec::new();
    loop {
        match pending.next_frame().await {
            Some(InboundFrame::Chunk(bytes)) => body_chunks.extend_from_slice(&bytes),
            Some(InboundFrame::End) => break,
            Some(InboundFrame::Error { message, .. }) => {
                error!(
                    request_id = %request_id,
                    handler_id = handler_id,
                    error = %message,
                    "worker error after HEAD"
                );
                break;
            }
            Some(_) => break,
            None => break,
        }
    }
    body_chunks
}

/// Streaming path: build a chunked HTTP response immediately and spawn a
/// drainer task that pumps subsequent CHUNK frames into the body stream.
///
/// Backpressure: the channel is bounded so a slow client pauses the drainer
/// (and, transitively, the worker) instead of letting bytes pile up in
/// gateway memory.
fn build_streaming_response(
    status: u16,
    headers: HashMap<String, String>,
    mut pending: PendingRequest,
    default_status: u16,
    request_id: &RequestId,
    handler_id: &str,
) -> Response {
    let actual_status = if status > 0 { status } else { default_status };
    let status_code = StatusCode::from_u16(actual_status).unwrap_or(StatusCode::OK);

    // 8 is enough to keep the worker producing while the client reads at
    // typical LAN speeds; larger values trade memory for fewer wakeups.
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);

    let drainer_request_id = *request_id;
    let drainer_handler_id = handler_id.to_string();
    tokio::spawn(async move {
        loop {
            match pending.next_frame().await {
                Some(InboundFrame::Chunk(bytes)) => {
                    if tx.send(Ok(bytes)).await.is_err() {
                        debug!(
                            request_id = %drainer_request_id,
                            handler_id = %drainer_handler_id,
                            "client closed stream; stopping drainer"
                        );
                        return;
                    }
                }
                Some(InboundFrame::End) => return,
                Some(InboundFrame::Error { message, .. }) => {
                    error!(
                        request_id = %drainer_request_id,
                        handler_id = %drainer_handler_id,
                        error = %message,
                        "worker error mid-stream"
                    );
                    let _ = tx
                        .send(Err(std::io::Error::other(message)))
                        .await;
                    return;
                }
                Some(_) => return,
                None => {
                    error!(
                        request_id = %drainer_request_id,
                        handler_id = %drainer_handler_id,
                        "worker disconnected mid-stream"
                    );
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionAborted,
                            "worker disconnected",
                        )))
                        .await;
                    return;
                }
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    let mut builder = Response::builder().status(status_code);
    for (key, value) in &headers {
        if let (Ok(name), Ok(val)) = (
            axum::http::header::HeaderName::from_bytes(key.as_bytes()),
            axum::http::header::HeaderValue::from_str(value),
        ) {
            builder = builder.header(name, val);
        }
    }
    if let Ok(val) = axum::http::header::HeaderValue::from_str(&request_id.to_string()) {
        builder = builder.header("x-request-id", val);
    }

    builder.body(body).unwrap_or_else(|e| {
        error!(request_id = %request_id, error = %e, "failed to build streaming response");
        make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_build_failed",
            &e.to_string(),
        )
    })
}

// ── Response building ─────────────────────────────────────────────────────────

fn build_http_response(
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    default_status: u16,
    request_id: &RequestId,
) -> Response {
    let actual_status = if status > 0 { status } else { default_status };
    let status_code = StatusCode::from_u16(actual_status).unwrap_or(StatusCode::OK);

    let mut response = if body.is_empty() {
        (status_code, "").into_response()
    } else {
        (status_code, body).into_response()
    };

    for (key, value) in &headers {
        if let (Ok(name), Ok(val)) = (
            axum::http::header::HeaderName::from_bytes(key.as_bytes()),
            axum::http::header::HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(name, val);
        }
    }

    if let Ok(val) = axum::http::header::HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert("x-request-id", val);
    }

    response
}

// ── Error helpers ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

fn make_error_response(status: StatusCode, code: &'static str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            code,
            message: message.to_string(),
        }),
    )
        .into_response()
}

fn make_validation_error_response(err: ValidationErrorResponse) -> Response {
    (StatusCode::BAD_REQUEST, Json(err)).into_response()
}

fn make_rate_limited_response(retry_after_secs: u64) -> Response {
    let mut resp = make_error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "Too many requests",
    );
    if let Ok(val) = header::HeaderValue::from_str(&retry_after_secs.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, val);
    }
    resp
}

/// Extract the client IP for rate limiting.
///
/// Prefers the leftmost address in `x-forwarded-for` (the original client as
/// seen by a proxy/load balancer). Falls back to `"unknown"` for direct
/// connections without that header.
fn resolve_peer_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
        let cors_registry = CorsRegistry::from_manifest(&manifest);
        let rate_limiter = RateLimiter::from_manifest(&manifest);
        let auth = AuthValidator::from_manifest(&manifest, "").unwrap();
        RouterState {
            manifest: Arc::new(manifest),
            schema_registry: Arc::new(schema_registry),
            cors: Arc::new(cors_registry),
            auth: Arc::new(auth),
            rate_limit: Arc::new(rate_limiter),
            pool: None, // no worker needed for routing / validation tests
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

    // ── Route matching ───────────────────────────────────────────────────────

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

    // ── Body validation ──────────────────────────────────────────────────────

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

        let body = axum::body::to_bytes(resp.into_body(), 10_000).await.unwrap();
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

        let body = axum::body::to_bytes(resp.into_body(), 10_000).await.unwrap();
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

    // ── Group prefix ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn group_prefix_route_is_reachable() {
        let state = test_router_state(&test_manifest());
        let router = build_manifest_router(state);

        // With no worker (pool: None), the handler returns 503, not 404.
        let req = Request::builder()
            .uri("/api/admin/users")
            .header("x-admin-token", "test-token")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Route counting ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn router_state_builds_successfully() {
        let state = test_router_state(&test_manifest());
        let routes = state.manifest.routes();
        assert_eq!(routes.len(), 6);
    }

    // ── Auth integration ─────────────────────────────────────────────────────

    const TEST_JWT_SECRET: &str = "router-test-secret";

    fn test_router_state_with_secret(manifest_json: &str, secret: &str) -> RouterState {
        let manifest = Manifest::from_json(manifest_json).unwrap();
        let schema_registry = SchemaRegistry::from_manifest(&manifest).unwrap();
        let cors_registry = CorsRegistry::from_manifest(&manifest);
        let rate_limiter = RateLimiter::from_manifest(&manifest);
        let auth = AuthValidator::from_manifest(&manifest, secret).unwrap();
        RouterState {
            manifest: Arc::new(manifest),
            schema_registry: Arc::new(schema_registry),
            cors: Arc::new(cors_registry),
            auth: Arc::new(auth),
            rate_limit: Arc::new(rate_limiter),
            pool: None,
        }
    }

    fn auth_manifest() -> String {
        serde_json::json!({
            "version": "1", "hash": "", "emitted_at": "",
            "services": [{
                "name": "profileService",
                "className": "ProfileService",
                "lifetime": "singleton",
                "dependencies": [],
                "handlers": [
                    {
                        "name": "getMe",
                        "method": "GET",
                        "path": "/me",
                        "status": 200,
                        "validate": false,
                        "extract": [],
                        "middleware": [{ "kind": "native", "name": "auth", "config": {} }]
                    },
                    {
                        "name": "getPublic",
                        "method": "GET",
                        "path": "/public",
                        "status": 200,
                        "validate": false,
                        "extract": []
                    }
                ]
            }],
            "middleware": [], "schemas": {}
        })
        .to_string()
    }

    fn mint_token(secret: &str, exp_offset_secs: i64) -> String {
        use jsonwebtoken::{encode, EncodingKey, Header};
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = serde_json::json!({ "sub": "test-user", "exp": now + exp_offset_secs });
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn protected_route_without_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);

        let req = Request::builder()
            .uri("/me")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(resp.into_body(), 10_000).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "jwt_missing");
    }

    #[tokio::test]
    async fn protected_route_with_invalid_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);

        let req = Request::builder()
            .uri("/me")
            .header("authorization", "Bearer not.a.real.token")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(resp.into_body(), 10_000).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "jwt_invalid");
    }

    #[tokio::test]
    async fn protected_route_with_expired_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);
        let token = mint_token(TEST_JWT_SECRET, -86400 * 100);

        let req = Request::builder()
            .uri("/me")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn protected_route_with_valid_token_passes_auth_reaches_worker_check() {
        // Auth passes → hits the no-pool check → 503 (not 401/403).
        // This confirms the auth layer is transparent to correctly-authed requests.
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);
        let token = mint_token(TEST_JWT_SECRET, 3600);

        let req = Request::builder()
            .uri("/me")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn unprotected_route_skips_auth_reaches_worker_check() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);

        let req = Request::builder()
            .uri("/public")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        // No 401 — went straight to the no-pool 503.
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn protected_route_with_wrong_secret_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state);
        // Token signed with a different secret.
        let token = mint_token("wrong-secret", 3600);

        let req = Request::builder()
            .uri("/me")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
