//! HTTP server construction and route handlers.
//!
//! Phase 1 has one hardcoded route (`POST /hello`) that forwards to the
//! worker's HelloService. Real routes arrive with the manifest emitter
//! in Phase 2.

use axum::Extension;
use axum::Router;
use axum::extract::{Json, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use sky_proto::v1::GreetRequest as ProtoGreetRequest;
use sky_runtime::{ClientError, RequestId};
use sky_worker::{HelloClient, Supervisor};
use std::sync::Arc;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{Span, error, info, info_span};

use crate::errors::HttpError;

/// Shared application state injected into every request handler.
#[derive(Clone)]
pub struct AppState {
    pub supervisor: Arc<Supervisor>,
}

/// Build the axum Router for Phase 1.
///
/// Phase 1 has a hardcoded `POST /hello` route. The manifest-driven
/// router arrives in Phase 2 (E2-S8).
///
/// Layer processing order (outermost first at runtime):
///   request_id_middleware → TraceLayer → RequestBodyLimitLayer → handler
pub fn build_router(state: AppState, body_limit_bytes: u64) -> Router {
    Router::new()
        .route("/hello", post(hello_handler))
        .layer(RequestBodyLimitLayer::new(
            usize::try_from(body_limit_bytes).unwrap_or(usize::MAX),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(RequestSpanMaker)
                .on_response(on_response_log)
                .on_failure(on_failure_log),
        )
        .layer(middleware::from_fn(request_id_middleware))
        .with_state(state)
}

/// Middleware that extracts or generates the request ID for each request.
///
/// On the way in: reads `X-Request-Id` from request headers; if present
/// and valid, reuses it (end-to-end client correlation); otherwise generates
/// a fresh UUIDv7. Inserts the `RequestId` into request extensions so
/// downstream layers and handlers can extract it without re-parsing.
///
/// On the way out: echoes `x-request-id` into response headers so clients
/// can correlate their request with gateway and worker logs.
async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = extract_or_create_request_id(request.headers());
    request.extensions_mut().insert(request_id);

    let mut response = next.run(request).await;

    response
        .headers_mut()
        .insert("x-request-id", format_header_value(request_id));

    response
}

/// Span factory for `TraceLayer` that includes the request ID, HTTP method,
/// and path. The request ID is read from extensions, populated by
/// `request_id_middleware` before `make_span` is called.
#[derive(Clone)]
struct RequestSpanMaker;

impl<B> tower_http::trace::MakeSpan<B> for RequestSpanMaker {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> Span {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .copied()
            .unwrap_or_else(RequestId::new);

        info_span!(
            "http.request",
            request_id = %request_id,
            http.method = request.method().as_str(),
            http.path = request.uri().path(),
            // Reserved slots filled in by on_response_log.
            http.status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        )
    }
}

/// Log a structured event when a response is sent.
///
/// Records HTTP status and wall-clock latency into the active span's
/// reserved fields, then emits an `info!` event so aggregators that
/// don't propagate span fields still see the data.
fn on_response_log(response: &axum::response::Response, latency: std::time::Duration, span: &Span) {
    let status = response.status().as_u16();
    let latency_ms = latency.as_millis();
    span.record("http.status", status);
    span.record("latency_ms", latency_ms);
    info!(http.status = status, latency_ms = latency_ms, "http response sent");
}

/// Log a structured event when a request fails at the transport level.
///
/// Fires for connection resets, body read errors, and similar transport
/// failures — not for handler errors that produce error responses, which
/// are logged in `HttpError::into_response`.
fn on_failure_log(
    error: tower_http::classify::ServerErrorsFailureClass,
    latency: std::time::Duration,
    _span: &Span,
) {
    error!(error = %error, latency_ms = latency.as_millis(), "http request failed");
}

/// JSON shape accepted by POST /hello.
///
/// In Phase 2 this would be derived from the manifest and its DTO
/// schema; for Phase 1 it is hardcoded to match HelloService.greet.
#[derive(Debug, Deserialize)]
struct GreetHttpRequest {
    name: String,
}

/// JSON shape returned by POST /hello.
#[derive(Debug, Serialize)]
struct GreetHttpResponse {
    message: String,
}

/// Handler for POST /hello.
///
/// Flow:
///   1. Read the request ID from extensions (inserted by `request_id_middleware`).
///   2. Deserialize the JSON body into the request DTO.
///   3. Call the worker's HelloService.greet via the typed client.
///   4. Return the response as JSON.
///
/// Structured log fields (request_id, http.method, http.path, http.status,
/// latency_ms) are attached by the enclosing TraceLayer span — the handler
/// does not create or enter spans itself.
async fn hello_handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    body: Result<Json<GreetHttpRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, HttpError> {
    info!("received greet request");

    let Json(req) =
        body.map_err(|err| ClientError::InvalidBody(format!("failed to parse JSON body: {err}")))?;

    let proto_request = ProtoGreetRequest { name: req.name };

    let client: HelloClient = state.supervisor.hello_client();
    let proto_response = client.greet(proto_request, request_id).await?;

    let body = GreetHttpResponse {
        message: proto_response.message,
    };

    Ok((StatusCode::OK, Json(body)).into_response())
}

/// Read `X-Request-Id` from inbound headers if present and valid;
/// otherwise generate a fresh ID.
///
/// A valid incoming ID is passed through unchanged for end-to-end
/// client correlation. An invalid or absent header produces a fresh UUIDv7.
fn extract_or_create_request_id(headers: &HeaderMap) -> RequestId {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| RequestId::try_from(s).ok())
        .unwrap_or_else(RequestId::new)
}

/// Format a RequestId as an HTTP header value.
fn format_header_value(id: RequestId) -> HeaderValue {
    // UUIDs are always valid ASCII so this expect is sound.
    HeaderValue::from_str(&id.to_string()).expect("request ID is always ASCII")
}
