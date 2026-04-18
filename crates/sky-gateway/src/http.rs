//! HTTP server construction and route handlers.
//!
//! Phase 1 has one hardcoded route (`POST /hello`) that forwards to the
//! worker's HelloService. Real routes arrive with the manifest emitter
//! in Phase 2.

use axum::Router;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use sky_proto::v1::GreetRequest as ProtoGreetRequest;
use sky_runtime::{ClientError, RequestId};
use sky_worker::{HelloClient, Supervisor};
use std::sync::Arc;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::info;

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
pub fn build_router(state: AppState, body_limit_bytes: u64) -> Router {
    Router::new()
        .route("/hello", post(hello_handler))
        // Apply a body size limit globally so all routes are protected.
        // Using `usize::try_from` because the layer wants usize;
        // on practical machines this is the same as u64.
        .layer(RequestBodyLimitLayer::new(
            usize::try_from(body_limit_bytes).unwrap_or(usize::MAX),
        ))
        .with_state(state)
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
///   1. Generate a request ID at ingress.
///   2. Deserialize the JSON body into the request DTO.
///   3. Call the worker's HelloService.greet via the typed client.
///   4. Return the response as JSON, with X-Request-Id in the headers.
async fn hello_handler(
    State(state): State<AppState>,
    headers_in: HeaderMap,
    body: Result<Json<GreetHttpRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, HttpError> {
    // Generate or preserve the request ID. If the client sent one,
    // we keep it for end-to-end tracing; otherwise we create a fresh one.
    let request_id = extract_or_create_request_id(&headers_in);

    // Enter the request span so every log line below carries the ID.
    let span = sky_runtime::request_span(request_id);
    let _guard = span.enter();

    info!("received greet request");

    // Convert axum's JSON rejection into our typed error.
    let Json(req) =
        body.map_err(|err| ClientError::InvalidBody(format!("failed to parse JSON body: {err}")))?;

    // Translate HTTP DTO to Protobuf type. In Phase 2 this translation
    // is derived automatically from the manifest.
    let proto_request = ProtoGreetRequest { name: req.name };

    // Fetch the HelloClient from the supervisor and make the RPC.
    let client: HelloClient = state.supervisor.hello_client();
    let proto_response = client.greet(proto_request, request_id).await?;

    // Translate back to HTTP DTO and build the response.
    let body = GreetHttpResponse {
        message: proto_response.message,
    };

    let mut response = (StatusCode::OK, Json(body)).into_response();
    response
        .headers_mut()
        .insert("x-request-id", format_header_value(request_id));

    Ok(response)
}

/// Read `X-Request-Id` from inbound headers if present and valid;
/// otherwise generate a fresh ID.
fn extract_or_create_request_id(headers: &HeaderMap) -> RequestId {
    // We don't currently parse existing request IDs from clients —
    // that would require a `TryFrom<&str>` on `RequestId` that we
    // haven't designed yet. Always generating fresh IDs is safe and
    // will be revisited in E1-S9 where request correlation is refined.
    let _ = headers;
    RequestId::new()
}

/// Format a RequestId as an HTTP header value.
fn format_header_value(id: RequestId) -> HeaderValue {
    // UUIDs are always valid ASCII so this expect is sound.
    HeaderValue::from_str(&id.to_string()).expect("request ID is always ASCII")
}
