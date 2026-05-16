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
use crate::config::{FrontendConfig, StaticFilesConfig};
use crate::cors::{CorsRegistry, apply_cors_headers};
use crate::manifest::Manifest;
use crate::proxy::DevProxyService;
use crate::rate_limit::{RateLimitOutcome, RateLimiter};
use crate::validation::{HandlerKey, SchemaRegistry, ValidationErrorResponse};
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{ConnectInfo, MatchedPath, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, options, patch, post, put};
use axum::{Extension, Json, Router, http};
use bytes::BytesMut;
use serde::Serialize;
use sky_runtime::RequestId;
use sky_worker::{InboundFrame, InvokePayload, PendingRequest, WorkerPool, WorkerSocket};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tower::{Layer, Service};
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{Span, debug, error, info, warn};
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
    /// Maximum request body size in bytes. Enforced inline during body collection.
    pub body_limit: usize,
    /// Maximum size for multipart / streaming uploads. Separate from `body_limit`
    /// so large file uploads aren't rejected by the JSON body cap. Default: 50 MB.
    pub upload_limit: usize,
    /// URL prefix of the frontend SPA, if one is configured. Used by the 404
    /// page to offer a "back" link. `None` when no `[frontend]` section exists.
    pub frontend_prefix: Option<String>,
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

    /// Whether JSON Schema validation is enabled for this handler's body.
    validate: bool,

    /// Whether this handler streams its response body to the client via
    /// HTTP chunked transfer encoding (true) or buffers it before
    /// responding (false). Drives the branch in `generic_handler`.
    stream_response_body: bool,

    stream_request_body: bool,

    /// Invocation timeout in milliseconds. Default: 30,000ms.
    timeout_ms: u64,
}

// ── Static file extension filter ─────────────────────────────────────────────

#[derive(Clone)]
struct BlockExtensionsLayer {
    excluded: Arc<Vec<String>>,
}

#[derive(Clone)]
struct BlockExtensions<S> {
    inner: S,
    excluded: Arc<Vec<String>>,
}

impl<S> Layer<S> for BlockExtensionsLayer {
    type Service = BlockExtensions<S>;
    fn layer(&self, inner: S) -> BlockExtensions<S> {
        BlockExtensions {
            inner,
            excluded: self.excluded.clone(),
        }
    }
}

impl<S, ReqBody> Service<http::Request<ReqBody>> for BlockExtensions<S>
where
    S: Service<http::Request<ReqBody>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = axum::response::Response;
    // Absorb inner errors into 500 responses so nest_service's Error=Infallible bound is met.
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.inner.poll_ready(cx) {
            Poll::Ready(_) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        if self
            .excluded
            .iter()
            .any(|ext| req.uri().path().ends_with(ext.as_str()))
        {
            return Box::pin(async { Ok(StatusCode::NOT_FOUND.into_response()) });
        }
        let fut = self.inner.call(req);
        Box::pin(async move {
            match fut.await {
                Ok(res) => Ok(res.into_response()),
                Err(_) => Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            }
        })
    }
}

// ── Router construction ───────────────────────────────────────────────────────

/// Build an axum `Router` from the manifest.
///
/// * `static_files` — mounts a `ServeDir` at a configurable path (always active).
/// * `frontend` + `dev_mode` — in dev mode, mounts a transparent reverse proxy at
///   `frontend.prefix` pointing to `frontend.dev_server`; in prod mode, serves
///   `frontend.output` with an SPA index.html fallback.
pub fn build_manifest_router(
    mut state: RouterState,
    static_files: Option<&StaticFilesConfig>,
    frontend: Option<&FrontendConfig>,
    dev_mode: bool,
) -> Router {
    state.frontend_prefix = frontend.map(|f| f.prefix.clone());
    let manifest = &state.manifest;
    let mut router = Router::new();
    let mut route_count = 0;
    // Track which paths have had an OPTIONS route registered (one per path, not per handler).
    let mut options_registered: std::collections::HashSet<String> =
        std::collections::HashSet::new();

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
                validate: handler.validate,
                stream_response_body: handler.stream_response_body,
                stream_request_body: handler.stream_request_body,
                timeout_ms: handler.timeout_ms.unwrap_or(30_000),
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

    if let Some(cfg) = static_files {
        let excluded = Arc::new(cfg.excluded_extensions.clone());
        let serve = BlockExtensionsLayer { excluded }.layer(ServeDir::new(&cfg.dir));
        router = router.nest_service(&cfg.path, serve);
        tracing::info!(path = %cfg.path, dir = %cfg.dir.display(), "static file serving enabled");
    }

    // Track whether we've already set a catch-all fallback via the frontend config,
    // so we don't override it with not_found_handler on the next line.
    let mut has_frontend_fallback = false;

    match (frontend, dev_mode) {
        (Some(cfg), true) if cfg.dev_server.is_some() => {
            let upstream = cfg.dev_server.as_deref().unwrap();
            let proxy = DevProxyService::new(upstream);
            if cfg.prefix == "/" {
                router = router.fallback_service(proxy);
                has_frontend_fallback = true;
            } else {
                router = router.nest_service(&cfg.prefix, proxy);
            }
            tracing::info!(
                upstream = upstream,
                prefix = %cfg.prefix,
                "frontend dev proxy enabled"
            );
        }
        (Some(cfg), false) => {
            use tower_http::services::ServeFile;
            let index = cfg.output.join("index.html");
            let serve = ServeDir::new(&cfg.output).fallback(ServeFile::new(index));
            if cfg.prefix == "/" {
                router = router.fallback_service(serve);
                has_frontend_fallback = true;
            } else {
                router = router.nest_service(&cfg.prefix, serve);
            }
            tracing::info!(
                dir = %cfg.output.display(),
                prefix = %cfg.prefix,
                "frontend static serving enabled (prod mode)"
            );
        }
        _ => {}
    }

    if !has_frontend_fallback {
        router = router.fallback(not_found_handler);
    }

    router
        .with_state(state)
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        path   = %request.uri().path(),
                        handler = tracing::field::Empty,
                        ip      = tracing::field::Empty,
                        status  = tracing::field::Empty,
                    )
                })
                // For streaming responses this fires at TTFB, not end-of-stream.
                .on_response(
                    |response: &axum::http::Response<_>, latency: Duration, span: &Span| {
                        span.record("status", response.status().as_u16());
                        info!(parent: span, duration_ms = latency.as_millis() as u64, "request");
                    },
                ),
        )
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
#[allow(clippy::too_many_arguments)]
async fn generic_handler(
    State(state): State<RouterState>,
    Extension(route): Extension<RouteInfo>,
    peer: Option<ConnectInfo<SocketAddr>>,
    method: Method,
    uri_path: http::Uri,
    Path(path_params): Path<HashMap<String, String>>,
    Query(query_params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let request_id = RequestId::new();
    let peer_ip = resolve_peer_ip(&headers, peer.map(|ConnectInfo(a)| a));
    let origin_str: Option<String> = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    Span::current().record("handler", route.handler_id.as_str());
    Span::current().record("ip", peer_ip.as_str());

    debug!(
        request_id = %request_id,
        handler_id = %route.handler_id,
        method = %method,
        "handling request"
    );

    // Detect multipart/form-data early — it uses upload_limit instead of body_limit
    // and is always buffered (never sent via InvokeBodyChunk streaming frames).
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let is_multipart = content_type.starts_with("multipart/form-data");

    // Fast rejection based on Content-Length — avoids reading any body bytes.
    if let Some(cl) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
    {
        let limit = if is_multipart {
            state.upload_limit
        } else {
            state.body_limit
        };
        if cl > limit {
            return make_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                &format!("Request body exceeds the {} byte limit", limit),
            );
        }
    }

    let mut stream = body.into_data_stream();

    // ── Body collection ──────────────────────────────────────────────────────

    // Multipart bodies are always buffered (Option A): the gateway reads them
    // up to upload_limit and passes raw bytes in the INVOKE frame. The TS
    // dispatcher's StreamedBody() fallback wraps them in a single-chunk iterable
    // so parseMultipart() works without any protocol changes.
    // Non-multipart streaming routes (SSE, binary proxying) still use the
    // InvokeBodyChunk streaming protocol.
    let use_streaming_body = route.stream_request_body && !is_multipart;

    let body_bytes: Vec<u8> = if use_streaming_body {
        Vec::new()
    } else if route.has_body && !stream.is_end_stream() {
        let limit = if is_multipart {
            state.upload_limit
        } else {
            state.body_limit
        };
        let mut collected_body = BytesMut::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    collected_body.extend_from_slice(&c);
                    if collected_body.len() > limit {
                        return make_error_response(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "payload_too_large",
                            &format!("Request body exceeds the {} byte limit", limit),
                        );
                    }
                }
                Err(e) => {
                    return make_error_response(
                        StatusCode::BAD_REQUEST,
                        "body_read_error",
                        &format!("Failed to read request body: {e}"),
                    );
                }
            }
        }

        // Skip JSON validation for multipart — the bytes are raw boundary-encoded data.
        if route.validate && !is_multipart {
            match serde_json::from_slice::<serde_json::Value>(&collected_body) {
                Ok(value) => {
                    if let Err(validation_err) = state.schema_registry.validate(&route.key, &value)
                    {
                        return make_validation_error_response(validation_err);
                    }
                }
                Err(e) => {
                    return make_error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_json",
                        &format!("Failed to parse request body as JSON: {e}"),
                    );
                }
            }
        }

        collected_body.to_vec()
    } else if route.has_body && stream.is_end_stream() {
        if route.validate {
            let empty = serde_json::Value::Object(serde_json::Map::new());
            if let Err(validation_err) = state.schema_registry.validate(&route.key, &empty) {
                return make_validation_error_response(validation_err);
            }
        }
        b"{}".to_vec()
    } else {
        Vec::new()
    };

    // ── Rate limiting ────────────────────────────────────────────────────────

    if let RateLimitOutcome::Denied { retry_after_secs } = state
        .rate_limit
        .check_and_record(&route.handler_id, &headers, &peer_ip)
        .await
    {
        return make_rate_limited_response(retry_after_secs);
    }

    // ── Authentication ───────────────────────────────────────────────────────

    let verified_claims = match state.auth.check(&route.handler_id, &headers) {
        AuthOutcome::Allowed { claims } => Some(claims),
        AuthOutcome::NotRequired => None,
        AuthOutcome::Denied {
            status,
            code,
            message,
        } => {
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

    header_map.insert("x-client-ip".to_string(), peer_ip);

    let payload = InvokePayload {
        handler_id: &route.handler_id,
        method: method.as_str(),
        path: uri_path.path(),
        params: &path_params,
        query: &query_params,
        headers: header_map,
        body: &body_bytes,
        stream: &use_streaming_body,
    };

    // ── Send INVOKE, collect response frames ─────────────────────────────────

    let stream_option = use_streaming_body.then_some(stream);

    let mut pending = match connection.invoke(&payload, "default", stream_option).await {
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

    // Streaming routes use the credit protocol so the worker can't flood the
    // gateway faster than the HTTP client drains. Non-streaming routes use
    // sendChunkDirect on the worker side, which bypasses credit entirely.
    if route.stream_response_body {
        const INITIAL_RESPONSE_CREDIT: u32 = 1024 * 1024; // 1 MiB
        let _ = connection
            .send_response_credit(pending.request_id, INITIAL_RESPONSE_CREDIT)
            .await;
    }

    // First frame must be RESPONSE_HEAD (or ERROR). Both code paths share this.
    let head_result = tokio::time::timeout(
        Duration::from_millis(route.timeout_ms),
        pending.next_frame(),
    )
    .await;

    let (status, response_headers) = match head_result {
        Err(_elapsed) => {
            warn!(
                request_id = %request_id,
                handler_id = %route.handler_id,
                timeout_ms = route.timeout_ms,
                "handler timed out"
            );
            // Tell the worker to stop processing so it can release credit
            // waiters and any streaming resources for this request.
            let _ = connection.cancel_request(pending.request_id).await;
            return make_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "handler_timeout",
                &format!("Handler did not respond within {}ms", route.timeout_ms),
            );
        }
        Ok(Some(InboundFrame::Head { status, headers })) => (status, headers),
        Ok(Some(InboundFrame::Error { message, .. })) => {
            error!(
                request_id = %request_id,
                handler_id = %route.handler_id,
                error = %message,
                "worker returned ERROR frame"
            );
            return make_error_response(StatusCode::BAD_GATEWAY, "worker_error", &message);
        }
        Ok(Some(_)) => {
            error!(request_id = %request_id, "unexpected first frame type");
            return make_error_response(
                StatusCode::BAD_GATEWAY,
                "protocol_error",
                "Unexpected first response frame",
            );
        }
        Ok(None) => {
            error!(request_id = %request_id, "worker connection closed before HEAD");
            return make_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "worker_disconnected",
                "Worker disconnected during request",
            );
        }
    };

    let mut response = if route.stream_response_body {
        build_streaming_response(
            connection.clone(),
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

    if let (Some(origin), Some(policy)) = (
        origin_str.as_deref(),
        state.cors.get_by_handler(&route.handler_id),
    ) {
        apply_cors_headers(policy, origin, response.headers_mut());
    }

    response
}

// ── Body drainers ─────────────────────────────────────────────────────────────

/// Buffered path: collect every chunk until END/ERROR, return the full body.
/// No credit is sent — non-streaming handlers use sendChunkDirect on the
/// worker side, which bypasses the credit protocol entirely.
async fn drain_buffered(
    pending: &mut PendingRequest,
    request_id: &RequestId,
    handler_id: &str,
) -> Vec<u8> {
    let mut body_chunks: Vec<u8> = Vec::new();
    loop {
        match pending.next_frame().await {
            Some(InboundFrame::Chunk(bytes)) => {
                body_chunks.extend_from_slice(&bytes);
            }
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
    connection: Arc<WorkerSocket>,
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
    let protocol_id = pending.request_id;
    tokio::spawn(async move {
        loop {
            match pending.next_frame().await {
                Some(InboundFrame::Chunk(bytes)) => {
                    let len = bytes.len() as u32;
                    if tx.send(Ok(bytes)).await.is_err() {
                        debug!(
                            request_id = %drainer_request_id,
                            handler_id = %drainer_handler_id,
                            "client closed stream; stopping drainer"
                        );
                        let _ = connection.cancel_request(protocol_id).await;
                        return;
                    }
                    // Replenish credit only after the chunk has been accepted
                    // into the HTTP body channel — the HTTP client paces this
                    // and thus paces the worker all the way back.
                    let _ = connection.send_response_credit(protocol_id, len).await;
                }
                Some(InboundFrame::End) => return,
                Some(InboundFrame::Error { message, .. }) => {
                    error!(
                        request_id = %drainer_request_id,
                        handler_id = %drainer_handler_id,
                        error = %message,
                        "worker error mid-stream"
                    );
                    let _ = tx.send(Err(std::io::Error::other(message))).await;
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

/// Extract the client IP for rate limiting and access logs.
///
/// Prefers the leftmost address in `x-forwarded-for` (the original client as
/// seen by a proxy/load balancer). Falls back to the TCP peer address for
/// direct connections, and finally to `"unknown"` if neither is available.
fn resolve_peer_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            peer.map(|a| a.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
}

// ── 404 fallback ──────────────────────────────────────────────────────────────

async fn not_found_handler(
    State(state): State<RouterState>,
    method: Method,
    uri_path: http::Uri,
) -> Response {
    let path = uri_path.path();
    // Return JSON for API-looking paths, HTML page for everything else.
    if path.starts_with("/api/") || path.starts_with("/health") {
        return make_error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("No route matched {} {}", method, path),
        );
    }
    let back_link = match &state.frontend_prefix {
        Some(prefix) => format!(r#"<a href="{prefix}">← Back to app</a>"#, prefix = prefix),
        None => String::new(),
    };
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>404 — Not Found</title>
  <style>
    *, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: #0f0f13; color: #e2e8f0;
      min-height: 100vh; display: flex; align-items: center; justify-content: center;
    }}
    .box {{ text-align: center; padding: 2rem; }}
    .code {{ font-size: 5rem; font-weight: 700; color: #6366f1; line-height: 1; }}
    .title {{ font-size: 1.25rem; font-weight: 600; color: #f8fafc; margin: 0.75rem 0 0.5rem; }}
    .path {{ font-family: "Fira Code", monospace; font-size: 0.875rem; color: #475569; margin-bottom: 1.5rem; }}
    a {{ color: #6366f1; text-decoration: none; font-size: 0.875rem; }}
    a:hover {{ text-decoration: underline; }}
  </style>
</head>
<body>
  <div class="box">
    <div class="code">404</div>
    <div class="title">Page not found</div>
    <div class="path">{path}</div>
    {back_link}
  </div>
</body>
</html>"#
    );
    (
        StatusCode::NOT_FOUND,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
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
            body_limit: 1024 * 1024,
            upload_limit: 50 * 1024 * 1024,
            frontend_prefix: None,
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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);

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
            body_limit: 1024 * 1024,
            upload_limit: 50 * 1024 * 1024,
            frontend_prefix: None,
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
        use jsonwebtoken::{EncodingKey, Header, encode};
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
        let router = build_manifest_router(state, None, None, false);

        let req = Request::builder().uri("/me").body(Body::empty()).unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "jwt_missing");
    }

    #[tokio::test]
    async fn protected_route_with_invalid_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state, None, None, false);

        let req = Request::builder()
            .uri("/me")
            .header("authorization", "Bearer not.a.real.token")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "jwt_invalid");
    }

    #[tokio::test]
    async fn protected_route_with_expired_token_returns_401() {
        let state = test_router_state_with_secret(&auth_manifest(), TEST_JWT_SECRET);
        let router = build_manifest_router(state, None, None, false);
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
        let router = build_manifest_router(state, None, None, false);
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
        let router = build_manifest_router(state, None, None, false);

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
        let router = build_manifest_router(state, None, None, false);
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
