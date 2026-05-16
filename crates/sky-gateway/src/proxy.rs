//! Transparent HTTP and WebSocket reverse proxy for the frontend dev server.
//!
//! Used in dev mode only (`sky dev` / `--dev` flag). Two code paths:
//!
//! * **HTTP** — rewrites the URI to point at the upstream dev server, forwards
//!   all headers (stripping hop-by-hop), and streams the response body back.
//!   A fresh HTTP/1.1 connection is opened per request (acceptable for localhost).
//!
//! * **WebSocket** — opens a raw TCP connection to the upstream, reconstructs
//!   the HTTP/1.1 Upgrade request byte-for-byte, reads the 101 response, then
//!   spawns a [`tokio::io::copy_bidirectional`] task to tunnel bytes between
//!   the client and upstream.  Sky never parses WS frames; HMR and any other
//!   WS sub-protocol work transparently.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri, header},
};
use hyper_util::rt::TokioIo;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tower::Service;
use tracing::{debug, warn};

/// Headers that must not be forwarded between proxy hops.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
];

// ── Service ───────────────────────────────────────────────────────────────────

/// Transparent dev-proxy service.
///
/// Clone-able so it can be used with `nest_service` / `fallback_service`.
#[derive(Clone)]
pub struct DevProxyService {
    upstream: Arc<String>,
}

impl DevProxyService {
    pub fn new(upstream: impl Into<String>) -> Self {
        Self {
            upstream: Arc::new(upstream.into()),
        }
    }
}

impl Service<Request<Body>> for DevProxyService {
    type Response = Response<Body>;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let upstream = self.upstream.clone();
        Box::pin(async move { Ok(proxy_request(upstream, req).await) })
    }
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

async fn proxy_request(upstream: Arc<String>, req: Request<Body>) -> Response<Body> {
    let is_ws_upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("websocket"))
        .unwrap_or(false);

    if is_ws_upgrade {
        tunnel_websocket(&upstream, req).await
    } else {
        forward_http(&upstream, req).await
    }
}

// ── HTTP forwarding ───────────────────────────────────────────────────────────

async fn forward_http(upstream: &str, req: Request<Body>) -> Response<Body> {
    let host_port = match host_port_from_url(upstream) {
        Some(hp) => hp,
        None => return bad_gateway("cannot parse upstream address"),
    };

    let stream = match TcpStream::connect(&host_port).await {
        Ok(s) => s,
        Err(e) => return bad_gateway(&format!("upstream connect failed: {e}")),
    };

    let io = TokioIo::new(stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(hc) => hc,
        Err(e) => return bad_gateway(&format!("HTTP/1.1 handshake failed: {e}")),
    };
    // Drive the connection in a background task; it exits when the response is done.
    tokio::spawn(conn);

    let (mut parts, body) = req.into_parts();

    // Keep only path+query for the request target so hyper sends origin-form
    // (e.g. `GET /src/main.tsx HTTP/1.1`). Sending the full absolute-form URI
    // causes Node.js / Vite to treat the URL as a literal path and fall back
    // to index.html for every asset request.
    let orig_pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    parts.uri = match orig_pq.parse::<Uri>() {
        Ok(u) => u,
        Err(e) => return bad_gateway(&format!("invalid upstream URI: {e}")),
    };

    // Override Host with the upstream's host:port.
    if let Ok(hv) = HeaderValue::from_str(&host_port) {
        parts.headers.insert(header::HOST, hv);
    }

    // Strip hop-by-hop headers before forwarding.
    for h in HOP_BY_HOP {
        parts.headers.remove(*h);
    }

    let upstream_req = Request::from_parts(parts, body);

    match sender.send_request(upstream_req).await {
        Ok(resp) => {
            let (resp_parts, resp_body) = resp.into_parts();
            Response::from_parts(resp_parts, Body::new(resp_body))
        }
        Err(e) => bad_gateway(&format!("upstream request failed: {e}")),
    }
}

// ── WebSocket tunnel ──────────────────────────────────────────────────────────

async fn tunnel_websocket(upstream: &str, mut req: Request<Body>) -> Response<Body> {
    // Capture the upgrade token BEFORE responding — resolves once 101 is sent.
    let on_upgrade = hyper::upgrade::on(&mut req);

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "/".to_string());

    let host_port = match host_port_from_url(upstream) {
        Some(hp) => hp,
        None => return bad_gateway("cannot parse upstream address for WS tunnel"),
    };

    // Connect to the upstream over raw TCP.
    let mut upstream_tcp = match TcpStream::connect(&host_port).await {
        Ok(s) => s,
        Err(e) => return bad_gateway(&format!("WS upstream connect failed: {e}")),
    };

    // Reconstruct and send the HTTP/1.1 Upgrade request to the upstream.
    let upgrade_req = build_ws_request(&path_and_query, &host_port, req.headers());
    if let Err(e) = upstream_tcp.write_all(upgrade_req.as_bytes()).await {
        return bad_gateway(&format!("write to upstream failed: {e}"));
    }

    // Read the upstream's 101 response (byte-by-byte until \r\n\r\n).
    let upstream_resp = match read_upgrade_response(&mut upstream_tcp).await {
        Ok(r) => r,
        Err(e) => return bad_gateway(&format!("bad upstream WS response: {e}")),
    };

    // Once we return the 101 response below, hyper completes the client-side
    // upgrade. The spawned task then gets the raw client stream and tunnels bytes.
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(client_io) => {
                debug!("WebSocket tunnel established");
                // Upgraded implements hyper's IO traits; TokioIo adapts it for tokio.
                let mut client_io = TokioIo::new(client_io);
                if let Err(e) =
                    tokio::io::copy_bidirectional(&mut client_io, &mut upstream_tcp).await
                {
                    // EOF and normal disconnects arrive as errors here; only log unexpected ones.
                    if e.kind() != io::ErrorKind::ConnectionReset
                        && e.kind() != io::ErrorKind::BrokenPipe
                        && e.kind() != io::ErrorKind::UnexpectedEof
                    {
                        warn!(error = %e, "WebSocket tunnel error");
                    }
                }
                debug!("WebSocket tunnel closed");
            }
            Err(e) => {
                warn!(error = %e, "WebSocket client upgrade failed");
            }
        }
    });

    build_101_response(&upstream_resp)
}

/// Reconstruct the HTTP/1.1 Upgrade request for the upstream.
fn build_ws_request(path_and_query: &str, host_port: &str, headers: &HeaderMap) -> String {
    let mut req = format!("GET {} HTTP/1.1\r\nHost: {}\r\n", path_and_query, host_port);
    for (name, value) in headers {
        let name_lc = name.as_str().to_ascii_lowercase();
        // Skip hop-by-hop (except Connection/Upgrade which are required for WS).
        if name_lc == "connection" || name_lc == "transfer-encoding" {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req.push_str(&format!("{}: {}\r\n", name.as_str(), v));
        }
    }
    req.push_str("\r\n");
    req
}

struct UpgradeResponse {
    headers: Vec<(String, String)>,
}

/// Read HTTP response headers byte-by-byte until `\r\n\r\n`, then verify 101.
async fn read_upgrade_response(stream: &mut TcpStream) -> Result<UpgradeResponse, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut byte = [0u8; 1];

    loop {
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|e| e.to_string())?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16_384 {
            return Err("upstream response headers too large".into());
        }
    }

    let text = std::str::from_utf8(&buf).map_err(|e| e.to_string())?;
    let mut lines = text.split("\r\n");

    let status_line = lines.next().ok_or("empty upstream response")?;
    if !status_line.contains("101") {
        return Err(format!("upstream did not return 101: {status_line}"));
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(": ") {
            headers.push((k.to_ascii_lowercase(), v.to_string()));
        }
    }

    Ok(UpgradeResponse { headers })
}

fn build_101_response(upstream: &UpgradeResponse) -> Response<Body> {
    let mut builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for (key, value) in &upstream.headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            builder = builder.header(name, val);
        }
    }
    builder
        .body(Body::empty())
        .unwrap_or_else(|_| bad_gateway("failed to build 101 response"))
}

// ── URL helpers ───────────────────────────────────────────────────────────────

/// Extract `host:port` from a URL like `http://localhost:5173`.
fn host_port_from_url(url: &str) -> Option<String> {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = stripped.split('/').next()?;
    if host.contains(':') {
        Some(host.to_string())
    } else if url.starts_with("https://") {
        Some(format!("{}:443", host))
    } else {
        Some(format!("{}:80", host))
    }
}

// ── Error response ────────────────────────────────────────────────────────────

fn bad_gateway(msg: &str) -> Response<Body> {
    let body = format!(r#"{{"code":"bad_gateway","message":{msg:?}}}"#);
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("bad_gateway response is always valid")
}
