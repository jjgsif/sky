//! Sky framing protocol — gateway-side transport layer.
//!
//! The gateway binds a Unix Domain Socket; TypeScript workers connect
//! to it as clients. Every exchange is multiplexed over a single
//! connection using a 10-byte binary frame header.
//!
//! # Frame format
//!
//! ```text
//! [0]     version     (u8)  — always 0x01
//! [1]     frame_type  (u8)  — see FrameType
//! [2..6]  request_id  (u32, big-endian)
//! [6..10] payload_len (u32, big-endian)
//! ```
//!
//! Structured payloads (INVOKE, RESPONSE_HEAD, ERROR) are MessagePack.
//! Body chunks (RESPONSE_CHUNK) are raw bytes.

use axum_core::body::BodyDataStream;
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures::StreamExt;
use rmp_serde as rmps;
use serde::{Deserialize, Serialize};
use sky_runtime::WorkerError;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, warn};

// ── Protocol constants ────────────────────────────────────────────────────────

pub const PROTOCOL_VERSION: u8 = 0x01;
const HEADER_SIZE: usize = 10;

// ── Frame types ───────────────────────────────────────────────────────────────

/// Frame type byte values used in the 10-byte frame header.
pub mod frame {
    pub const INVOKE: u8 = 0x01;
    pub const RESPONSE_HEAD: u8 = 0x02;
    pub const RESPONSE_CHUNK: u8 = 0x03;
    pub const RESPONSE_END: u8 = 0x04;
    pub const ERROR: u8 = 0x05;
    pub const PING: u8 = 0x06;
    pub const PONG: u8 = 0x07;
    pub const DRAIN: u8 = 0x08;
    pub const INVOKE_BODY_CHUNK: u8 = 0x09;
    pub const INVOKE_END: u8 = 0x0A;
    pub const INVOKE_CREDIT: u8 = 0x0B;
    pub const INVOKE_CANCEL: u8 = 0x0C;
    pub const RESPONSE_CREDIT: u8 = 0x0D;
    pub const AUTH_CHALLENGE: u8 = 0x0E;
    pub const AUTH_RESPONSE: u8 = 0x0F;
}

// ── Public types ──────────────────────────────────────────────────────────────

/// An inbound response frame routed to a specific in-flight request.
pub enum InboundFrame {
    Head {
        status: u16,
        headers: HashMap<String, String>,
    },
    Chunk(Bytes),
    End,
    Error {
        code: String,
        message: String,
    },
}

/// Payload for an INVOKE frame (MessagePack-serialized).
///
/// All fields are sent to the worker, which filters them based on its
/// extract descriptors. The gateway sends everything; the worker picks
/// what it needs.
#[derive(Serialize)]
pub struct InvokePayload<'a> {
    pub handler_id: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub headers: HashMap<String, String>,
    #[serde(with = "serde_bytes")]
    pub body: &'a [u8],
    pub stream: &'a bool,
}

pub struct WorkerHealthState {
    pub last_frame_ms_ago: u64,
    pub inflight: usize,
    pub frames_routed: u64,
    pub is_draining: bool,
}

/// Receiver half for one in-flight INVOKE — yields response frames.
pub struct PendingRequest {
    pub request_id: u32,
    rx: mpsc::UnboundedReceiver<InboundFrame>,
}

impl PendingRequest {
    /// Await the next response frame. Returns `None` when the channel closes.
    pub async fn next_frame(&mut self) -> Option<InboundFrame> {
        self.rx.recv().await
    }
}

// ── WorkerSocket ──────────────────────────────────────────────────────────────

/// An established, multiplexed connection with a TypeScript worker.
///
/// Multiple concurrent INVOKE requests share the same connection,
/// distinguished by their `request_id` in every frame header.
///
/// `WorkerSocket` is cheap to clone — all state lives behind an `Arc`
/// that is shared by the background read task.
pub struct WorkerSocket {
    write_tx: mpsc::Sender<Bytes>,
    inflight: Arc<DashMap<u32, mpsc::UnboundedSender<InboundFrame>>>,
    ping_tx: Mutex<Option<oneshot::Sender<()>>>,
    next_id: AtomicU32,
    stream_tx: mpsc::Sender<(u32, BodyDataStream)>,

    // Health state — updated by normal frame traffic
    pub last_frame_at: AtomicU64, // Unix timestamp ms of last inbound frame
    pub frames_routed: AtomicU64, // total frames routed (already exists)
    pub inflight_count: AtomicUsize, // current in-flight request count

    // Diagnostics (existing)
    pub write_lock_wait_ns: AtomicU64,
    pub inflight_lock_wait_ns: AtomicU64,
    pub frames_dropped: AtomicU64,
}

impl WorkerSocket {
    fn from_stream(stream: UnixStream) -> Arc<Self> {
        let (read, write) = stream.into_split();
        let (stream_tx, stream_rx) = mpsc::channel::<(u32, BodyDataStream)>(256);
        let (write_tx, write_rx) = mpsc::channel::<Bytes>(4096);
        let socket = Arc::new(Self {
            write_tx,
            inflight: Arc::new(DashMap::new()),
            ping_tx: Mutex::new(None),
            next_id: AtomicU32::new(1),
            last_frame_at: AtomicU64::new(0),
            frames_routed: AtomicU64::new(0),
            inflight_count: AtomicUsize::new(0),
            write_lock_wait_ns: AtomicU64::new(0),
            inflight_lock_wait_ns: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
            stream_tx,
        });
        tokio::spawn(read_loop(socket.clone(), read, stream_rx));
        tokio::spawn(write_loop(write_rx, write));
        socket
    }

    /// Send a PING and wait for PONG. Used for readiness health checks.
    pub async fn ping(&self, pool: &str, timeout: std::time::Duration) -> Result<(), WorkerError> {
        let (tx, rx) = oneshot::channel();
        *self.ping_tx.lock().await = Some(tx);

        self.write_frame(frame::PING, 0, &[])
            .await
            .map_err(|e| WorkerError::Unreachable {
                pool: pool.to_string(),
                reason: format!("failed to send PING: {e}"),
            })?;

        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| WorkerError::ReadinessTimeout {
                pool: pool.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            })?
            .map_err(|_| WorkerError::Unreachable {
                pool: pool.to_string(),
                reason: "worker disconnected before PONG".to_string(),
            })
    }

    /// Send a DRAIN frame to initiate graceful worker shutdown.
    pub async fn drain(&self) -> std::io::Result<()> {
        self.write_frame(frame::DRAIN, 0, &[]).await
    }

    /// Send an INVOKE_CANCEL frame to tell the worker to abandon a request.
    ///
    /// Used when the HTTP client disconnects mid-response so the worker can
    /// release its credit waiters and stop processing the abandoned request.
    pub async fn cancel_request(&self, request_id: u32) -> std::io::Result<()> {
        self.write_frame(frame::INVOKE_CANCEL, request_id, &[])
            .await
    }

    /// Send a RESPONSE_CREDIT frame authorising the worker to send up to
    /// `allowed_bytes` more bytes of response body for `request_id`. Used by
    /// the router's streaming drainer to pace the worker.
    pub async fn send_response_credit(
        &self,
        request_id: u32,
        allowed_bytes: u32,
    ) -> std::io::Result<()> {
        #[derive(Serialize)]
        struct CreditPayload {
            #[serde(rename = "allowedBytes")]
            allowed_bytes: u32,
        }
        let payload = rmps::to_vec_named(&CreditPayload { allowed_bytes })
            .map_err(|e| std::io::Error::other(format!("encode credit: {e}")))?;
        self.write_frame(frame::RESPONSE_CREDIT, request_id, &payload)
            .await
    }

    /// Send an INVOKE frame and return a [`PendingRequest`] to collect
    /// the worker's response frames.
    pub async fn invoke(
        &self,
        payload: &InvokePayload<'_>,
        pool: &str,
        streaming_body: Option<BodyDataStream>,
    ) -> Result<PendingRequest, WorkerError> {
        // Allocate a unique request_id. Skip 0 (reserved for PING).
        let mut id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            id = self.next_id.fetch_add(1, Ordering::Relaxed);
        }

        let encoded = rmps::to_vec_named(payload).map_err(|e| WorkerError::ProtocolViolation {
            pool: pool.to_string(),
            detail: format!("INVOKE encode failed: {e}"),
        })?;

        let (tx, rx) = mpsc::unbounded_channel::<InboundFrame>();
        self.inflight.insert(id, tx);
        self.inflight_count.fetch_add(1, Ordering::Relaxed);

        if let Some(body) = streaming_body {
            let _ = self.stream_tx.send((id, body)).await;
            // Yield so the read_loop can register the stream before the INVOKE
            // frame is written — prevents a race where the worker receives the
            // frame and immediately sends INVOKE_CREDIT before the gateway has
            // registered the credit sender.
            tokio::task::yield_now().await;
        }

        if let Err(e) = self.write_frame(frame::INVOKE, id, &encoded).await {
            self.inflight.remove(&id);
            self.inflight_count.fetch_sub(1, Ordering::Relaxed);
            return Err(WorkerError::Unreachable {
                pool: pool.to_string(),
                reason: format!("failed to send INVOKE: {e}"),
            });
        }

        Ok(PendingRequest { request_id: id, rx })
    }

    pub fn health_state(&self) -> WorkerHealthState {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let last = self.last_frame_at.load(Ordering::Relaxed);
        let last_frame_ms_ago = if last == 0 {
            u64::MAX // never seen a frame — worker just started
        } else {
            now_ms.saturating_sub(last)
        };

        WorkerHealthState {
            last_frame_ms_ago,
            inflight: self.inflight_count.load(Ordering::Relaxed),
            frames_routed: self.frames_routed.load(Ordering::Relaxed),
            is_draining: false, // add a flag if needed
        }
    }
    // ── Internal ──────────────────────────────────────────────────────────

    async fn write_frame(
        &self,
        frame_type: u8,
        request_id: u32,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let mut buf = BytesMut::with_capacity(HEADER_SIZE + payload.len());
        buf.extend_from_slice(&[
            PROTOCOL_VERSION,
            frame_type,
            (request_id >> 24) as u8,
            (request_id >> 16) as u8,
            (request_id >> 8) as u8,
            request_id as u8,
            (payload.len() as u32 >> 24) as u8,
            (payload.len() as u32 >> 16) as u8,
            (payload.len() as u32 >> 8) as u8,
            payload.len() as u8,
        ]);
        if !payload.is_empty() {
            buf.extend_from_slice(payload);
        }

        let t = std::time::Instant::now();
        let result = self.write_tx.try_send(buf.freeze());
        self.write_lock_wait_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);

        result.map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "write task closed"))
    }

    async fn handle_frame(
        &self,
        frame_type: u8,
        request_id: u32,
        payload: Bytes,
        body_credit_senders: &mut HashMap<u32, mpsc::Sender<()>>,
    ) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_frame_at.store(now_ms, Ordering::Relaxed);

        match frame_type {
            frame::RESPONSE_HEAD => {
                #[derive(Deserialize)]
                struct Head {
                    status: u16,
                    headers: HashMap<String, String>,
                }
                match rmps::from_slice::<Head>(&payload) {
                    Ok(h) => {
                        self.route(
                            request_id,
                            InboundFrame::Head {
                                status: h.status,
                                headers: h.headers,
                            },
                        );
                    }
                    Err(e) => warn!("failed to decode RESPONSE_HEAD: {e}"),
                }
            }
            frame::RESPONSE_CHUNK => {
                self.route(request_id, InboundFrame::Chunk(payload));
            }
            frame::RESPONSE_END => {
                self.route(request_id, InboundFrame::End);
                // route() may have already removed the entry (receiver dropped).
                // Only decrement if the entry was actually present here.
                if self.inflight.remove(&request_id).is_some() {
                    self.inflight_count.fetch_sub(1, Ordering::Relaxed);
                }
                body_credit_senders.remove(&request_id);
            }
            frame::ERROR => {
                #[derive(Deserialize)]
                struct Err {
                    code: String,
                    message: String,
                }
                match rmps::from_slice::<Err>(&payload) {
                    Ok(e) => {
                        self.route(
                            request_id,
                            InboundFrame::Error {
                                code: e.code,
                                message: e.message,
                            },
                        );
                        if self.inflight.remove(&request_id).is_some() {
                            self.inflight_count.fetch_sub(1, Ordering::Relaxed);
                        }
                        body_credit_senders.remove(&request_id);
                    }
                    Err(e) => warn!("failed to decode ERROR frame: {e}"),
                }
            }
            frame::PONG => {
                if let Some(tx) = self.ping_tx.lock().await.take() {
                    let _ = tx.send(());
                }
            }
            frame::INVOKE_CREDIT => {
                if let Some(tx) = body_credit_senders.get(&request_id) {
                    // Non-blocking signal — the dedicated stream_body task owns
                    // the slow stream.next().await so the read_loop never blocks.
                    if tx.try_send(()).is_err() {
                        warn!(request_id, "INVOKE_CREDIT dropped: body task full or gone");
                    }
                } else {
                    warn!(request_id, "INVOKE_CREDIT for unknown stream");
                }
            }
            other => {
                debug!("unexpected inbound frame type: {:#04x}", other);
            }
        }
    }

    fn route(&self, request_id: u32, frame: InboundFrame) {
        let t = std::time::Instant::now();
        // The Ref from inflight.get() holds a shard read lock.  We must drop
        // that Ref *before* calling inflight.remove() (which needs the shard
        // write lock), otherwise we deadlock on the same parking_lot shard.
        let send_result: Option<Result<(), _>> = {
            let guard = self.inflight.get(&request_id);
            guard.map(|tx| tx.send(frame))
            // guard drops here → read lock released
        };
        self.inflight_lock_wait_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);

        match send_result {
            None => {}
            Some(Ok(())) => {
                self.frames_routed.fetch_add(1, Ordering::Relaxed);
            }
            Some(Err(_)) if self.inflight.remove(&request_id).is_some() => {
                // Receiver dropped (gateway timed out) — clean up the entry
                // and correct the in-flight counter.
                self.inflight_count.fetch_sub(1, Ordering::Relaxed);
            }
            Some(Err(_)) => {}
        }
    }

    async fn on_closed(&self) {
        self.inflight.retain(|_, tx| {
            let _ = tx.send(InboundFrame::Error {
                code: "socket_closed".to_string(),
                message: "worker connection closed unexpectedly".to_string(),
            });
            false
        });
        // Unblock any pending ping.
        if let Some(tx) = self.ping_tx.lock().await.take() {
            drop(tx);
        }
    }
}

// ── SkyListener ───────────────────────────────────────────────────────────────

/// Binds a Unix Domain Socket and accepts incoming worker connections.
///
/// The gateway creates the socket; workers connect as clients. The
/// listener remains bound across worker restarts so reconnection is
/// transparent.
pub struct SkyListener {
    listener: UnixListener,
}

// ── Auth handshake helpers ────────────────────────────────────────────────────

async fn write_raw_frame(
    stream: &mut UnixStream,
    frame_type: u8,
    request_id: u32,
    payload: &[u8],
) -> std::io::Result<()> {
    let plen = payload.len() as u32;
    let mut buf = BytesMut::with_capacity(HEADER_SIZE + payload.len());
    buf.extend_from_slice(&[
        PROTOCOL_VERSION,
        frame_type,
        (request_id >> 24) as u8,
        (request_id >> 16) as u8,
        (request_id >> 8) as u8,
        request_id as u8,
        (plen >> 24) as u8,
        (plen >> 16) as u8,
        (plen >> 8) as u8,
        plen as u8,
    ]);
    buf.extend_from_slice(payload);
    stream.write_all(&buf).await
}

async fn read_raw_frame(stream: &mut UnixStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header).await?;
    if header[0] != PROTOCOL_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "protocol version mismatch during handshake: got 0x{:02x}",
                header[0]
            ),
        ));
    }
    let frame_type = header[1];
    let payload_len = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await?;
    Ok((frame_type, payload))
}

async fn authenticate_inbound(stream: &mut UnixStream, secret: &[u8]) -> std::io::Result<()> {
    use hmac::{Hmac, Mac};
    use rand::RngCore;
    use sha2::Sha256;

    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    write_raw_frame(stream, frame::AUTH_CHALLENGE, 0, &nonce).await?;

    let (ft, payload) = read_raw_frame(stream).await?;
    if ft != frame::AUTH_RESPONSE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("auth: expected AUTH_RESPONSE (0x0F), got 0x{ft:02x}"),
        ));
    }

    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    mac.update(&nonce);
    mac.verify_slice(&payload).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "auth: HMAC mismatch")
    })
}

impl SkyListener {
    /// Bind a UDS listener at `path`. Removes any stale socket file first
    /// and creates parent directories as needed.
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(path)?;
        Ok(Self { listener })
    }

    /// Block until the worker connects, authenticate via HMAC-SHA256 challenge-response,
    /// then return the [`WorkerSocket`].
    pub async fn accept(&self, secret: &[u8]) -> std::io::Result<Arc<WorkerSocket>> {
        let (mut stream, _addr) = self.listener.accept().await?;
        authenticate_inbound(&mut stream, secret).await?;
        Ok(WorkerSocket::from_stream(stream))
    }
}

// ── Background read loop ──────────────────────────────────────────────────────

async fn read_loop(
    socket: Arc<WorkerSocket>,
    mut read: OwnedReadHalf,
    mut stream_rx: mpsc::Receiver<(u32, BodyDataStream)>,
) {
    // Maps request_id → signal channel for the dedicated stream_body task.
    // INVOKE_CREDIT frames send () here; the task owns stream.next().await
    // so the read_loop is never blocked on HTTP body I/O.
    let mut body_credit_senders: HashMap<u32, mpsc::Sender<()>> = HashMap::new();

    let mut accum = BytesMut::with_capacity(8 * 4096);
    let mut tmp = vec![0u8; 8 * 4096];

    loop {
        tokio::select! {
            result = read.read(&mut tmp) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => accum.extend_from_slice(&tmp[..n]),
                    Err(e) => {
                        warn!("worker socket read error: {e}");
                        break;
                    }
                }
            }
            Some((id, body_stream)) = stream_rx.recv() => {
                let (credit_tx, credit_rx) = mpsc::channel::<()>(32);
                body_credit_senders.insert(id, credit_tx);
                tokio::spawn(stream_body(socket.clone(), id, body_stream, credit_rx));
            }
        }

        // Drain any streams that arrived concurrently with the socket read.
        while let Ok((id, body_stream)) = stream_rx.try_recv() {
            let (credit_tx, credit_rx) = mpsc::channel::<()>(32);
            body_credit_senders.insert(id, credit_tx);
            tokio::spawn(stream_body(socket.clone(), id, body_stream, credit_rx));
        }

        // Parse all complete frames before awaiting the next socket read.
        // Drainer tasks for streaming responses are scheduled when the outer
        // select! yields between reads — no explicit yield_now needed here.
        loop {
            if accum.len() < HEADER_SIZE {
                break;
            }
            if accum[0] != PROTOCOL_VERSION {
                warn!(
                    "protocol version mismatch: got {}, expected {}. Buffer prefix: {:02x?}",
                    accum[0],
                    PROTOCOL_VERSION,
                    &accum[..accum.len().min(16)]
                );
                break;
            }
            let frame_type = accum[1];
            let request_id = u32::from_be_bytes([accum[2], accum[3], accum[4], accum[5]]);
            let payload_len = u32::from_be_bytes([accum[6], accum[7], accum[8], accum[9]]) as usize;

            if accum.len() < HEADER_SIZE + payload_len {
                break;
            }

            let _ = accum.split_to(HEADER_SIZE);
            let payload = accum.split_to(payload_len).freeze();

            socket
                .handle_frame(frame_type, request_id, payload, &mut body_credit_senders)
                .await;
        }
    }

    socket.on_closed().await;
}

/// Dedicated task per streaming upload request.
///
/// Owns `stream.next().await` so the read_loop is never blocked on HTTP I/O.
/// Each `()` received on `credit_rx` means the worker consumed a chunk and
/// is ready for the next one; we fetch it and write an INVOKE_BODY_CHUNK frame.
/// The task exits when `credit_rx` closes (request completed or cancelled).
async fn stream_body(
    socket: Arc<WorkerSocket>,
    request_id: u32,
    mut stream: BodyDataStream,
    mut credit_rx: mpsc::Receiver<()>,
) {
    while credit_rx.recv().await.is_some() {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if let Err(e) = socket
                    .write_frame(frame::INVOKE_BODY_CHUNK, request_id, &chunk)
                    .await
                {
                    warn!(request_id, error = %e, "failed to write INVOKE_BODY_CHUNK");
                    return;
                }
            }
            Some(Err(e)) => {
                warn!(request_id, error = %e, "body stream read error");
                let _ = socket
                    .write_frame(frame::INVOKE_CANCEL, request_id, &[])
                    .await;
                return;
            }
            None => {
                let _ = socket.write_frame(frame::INVOKE_END, request_id, &[]).await;
                return;
            }
        }
    }
}

async fn write_loop(mut rx: mpsc::Receiver<Bytes>, mut write: OwnedWriteHalf) {
    while let Some(frame) = rx.recv().await {
        if write.write_all(&frame).await.is_err() {
            break;
        }
    }
    // rx dropped here — any subsequent send() calls will return Err
    // which write_frame maps to BrokenPipe
}
