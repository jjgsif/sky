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

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
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
}

pub struct WorkerHealthState {
    pub last_frame_ms_ago: u64,
    pub inflight: usize,
    pub frames_routed: u64,
    pub is_draining: bool,
}

/// Receiver half for one in-flight INVOKE — yields response frames.
pub struct PendingRequest {
    rx: mpsc::Receiver<InboundFrame>,
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
    inflight: Arc<DashMap<u32, mpsc::Sender<InboundFrame>>>,
    ping_tx: Mutex<Option<oneshot::Sender<()>>>,
    next_id: AtomicU32,

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
        });
        tokio::spawn(read_loop(socket.clone(), read));
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

    /// Send an INVOKE frame and return a [`PendingRequest`] to collect
    /// the worker's response frames.
    pub async fn invoke(
        &self,
        payload: &InvokePayload<'_>,
        pool: &str,
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

        let (tx, rx) = mpsc::channel::<InboundFrame>(256);
        self.inflight.insert(id, tx);
        self.inflight_count.fetch_add(1, Ordering::Relaxed);

        if let Err(e) = self.write_frame(frame::INVOKE, id, &encoded).await {
            self.inflight.remove(&id);
            return Err(WorkerError::Unreachable {
                pool: pool.to_string(),
                reason: format!("failed to send INVOKE: {e}"),
            });
        }

        Ok(PendingRequest { rx })
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

    async fn handle_frame(&self, frame_type: u8, request_id: u32, payload: Bytes) {
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
                        )
                        .await;
                    }
                    Err(e) => warn!("failed to decode RESPONSE_HEAD: {e}"),
                }
            }
            frame::RESPONSE_CHUNK => {
                self.route(request_id, InboundFrame::Chunk(payload)).await;
            }
            frame::RESPONSE_END => {
                self.route(request_id, InboundFrame::End).await;
                self.inflight.remove(&request_id);
                self.inflight_count.fetch_sub(1, Ordering::Relaxed);
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
                        )
                        .await;
                        self.inflight.remove(&request_id);
                        self.inflight_count.fetch_sub(1, Ordering::Relaxed);
                    }
                    Err(e) => warn!("failed to decode ERROR frame: {e}"),
                }
            }
            frame::PONG => {
                if let Some(tx) = self.ping_tx.lock().await.take() {
                    let _ = tx.send(());
                }
            }
            other => {
                debug!("unexpected inbound frame type: {:#04x}", other);
            }
        }
    }

    async fn route(&self, request_id: u32, frame: InboundFrame) {
        // Measure and release the lock as fast as possible —
        // just extract the sender, nothing else
        let t = std::time::Instant::now();
        let tx = self.inflight.get(&request_id);
        self.inflight_lock_wait_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);

        // Lock is now released — send without holding it
        if let Some(tx) = tx {
            self.frames_routed.fetch_add(1, Ordering::Relaxed);
            if tx.send(frame).await.is_err() {
                // Receiver dropped — request cancelled, clean up
                self.inflight.remove(&request_id);
            }
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

    /// Block until the worker connects, then return the [`WorkerSocket`].
    pub async fn accept(&self) -> std::io::Result<Arc<WorkerSocket>> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(WorkerSocket::from_stream(stream))
    }
}

// ── Background read loop ──────────────────────────────────────────────────────

async fn read_loop(socket: Arc<WorkerSocket>, mut read: OwnedReadHalf) {
    let mut accum = BytesMut::with_capacity(8 * 4096);
    let mut tmp = vec![0u8; 8 * 4096];

    loop {
        match read.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => accum.extend_from_slice(&tmp[..n]),
            Err(e) => {
                warn!("worker socket read error: {e}");
                break;
            }
        }

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

            socket.handle_frame(frame_type, request_id, payload).await;
        }
    }

    socket.on_closed().await;
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
