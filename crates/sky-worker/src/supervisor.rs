//! Worker process supervisor.
//!
//! Owns the lifecycle of a TypeScript Bun worker: spawning, readiness
//! checking via PING/PONG, crash detection, restart with backoff, and
//! graceful shutdown via the DRAIN frame.
//!
//! # Socket numbering
//!
//! The gateway binds the socket using the same convention as the TS worker:
//!
//!   `/tmp/sky/workers/sky-worker-{id}.sock`
//!
//! The `worker_id` string (e.g. `"1"`, `"2"`) is the sole source of truth
//! for the socket path on both sides. The gateway binds the socket first;
//! the worker connects to it as a client. On restart the listener stays
//! bound so the worker can reconnect without rebinding.

use crate::config::WorkerConfig;
use crate::restart_policy::{FailureOutcome, RestartPolicy};
use crate::transport::{SkyListener, WorkerSocket};
use arc_swap::ArcSwap;
use sky_runtime::WorkerError;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Convention-derived socket path for a worker.
///
/// Both the gateway and the TS worker use this same formula so neither
/// side needs an explicit socket-path configuration field.
pub fn socket_path_for_id(worker_id: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/sky/workers/sky-worker-{worker_id}.sock"))
}

/// Owns a running worker process and its Sky framing connection.
///
/// Construct via [`Supervisor::start`], use via [`Supervisor::connection`],
/// clean up via [`Supervisor::shutdown`].
pub struct Supervisor {
    /// The current active connection to the worker.
    /// Updated atomically on each restart without blocking in-flight RPCs.
    pub connection: Arc<ArcSwap<WorkerSocket>>,
    _config: WorkerConfig,
    pool_name: String,
    shutdown_token: CancellationToken,
    monitor_handle: JoinHandle<()>,
}

impl Supervisor {
    /// Start the worker supervisor.
    ///
    /// `worker_id` numbers the socket: `/tmp/sky/workers/sky-worker-{id}.sock`.
    /// Pass `"1"` for a single-worker setup; future pool managers will assign
    /// unique IDs per slot.
    ///
    /// Steps:
    /// 1. Bind the socket (derived from `worker_id`).
    /// 2. Spawn the Bun worker with `SKY_WORKER_ID={worker_id}`.
    /// 3. Wait for the worker to connect and respond to PING.
    pub async fn start(
        mut config: WorkerConfig,
        worker_id: impl Into<String>,
    ) -> Result<Self, WorkerError> {
        let worker_id = worker_id.into();
        let pool_name = format!("worker-{worker_id}");

        config.validate().map_err(|e| WorkerError::Unreachable {
            pool: pool_name.clone(),
            reason: format!("invalid config: {e}"),
        })?;

        // Generate a per-instance secret for worker authentication.
        {
            use rand::RngCore;
            rand::rng().fill_bytes(&mut config.auth_secret);
        }

        // Both Rust and TS derive the socket path from the worker ID.
        let sock_path = socket_path_for_id(&worker_id);
        let listener = SkyListener::bind(&sock_path).map_err(|e| WorkerError::Unreachable {
            pool: pool_name.clone(),
            reason: format!("failed to bind socket at {}: {e}", sock_path.display()),
        })?;
        let shutdown_token = CancellationToken::new();
        let (child, socket) = spawn_and_ready(
            &config,
            &worker_id,
            &pool_name,
            &listener,
            shutdown_token.clone(),
        )
        .await?;
        let connection_swap = Arc::new(ArcSwap::new(socket));
        let restart_policy = RestartPolicy::new(&config);

        let monitor_handle = tokio::spawn({
            let shutdown_token = shutdown_token.clone();
            let config = config.clone();
            let worker_id = worker_id.clone();
            let pool_name = pool_name.clone();
            let connection = connection_swap.clone();
            async move {
                monitor_loop(
                    child,
                    restart_policy,
                    MonitorInit {
                        config,
                        connection,
                        listener,
                        cancel_token: shutdown_token,
                        worker_id,
                        pool_name,
                    },
                )
                .await;
            }
        });

        Ok(Self {
            connection: connection_swap,
            _config: config,
            pool_name,
            shutdown_token,
            monitor_handle,
        })
    }

    /// Return a handle to the current active worker connection.
    #[must_use]
    pub fn connection(&self) -> Arc<WorkerSocket> {
        self.connection.load_full()
    }

    /// Gracefully shut down the worker. Consumes self.
    ///
    /// Sends a DRAIN frame to the worker, then waits for the monitor
    /// task to complete. The monitor force-kills the worker if it
    /// doesn't exit within the grace period.
    pub async fn shutdown(self) -> Result<(), WorkerError> {
        info!(pool = %self.pool_name, "initiating worker shutdown");

        // Cancel the token — tells the monitor not to restart the worker.
        self.shutdown_token.cancel();

        // Send DRAIN to let the worker finish in-flight requests.
        let socket = self.connection.load_full();
        if let Err(e) = socket.drain().await {
            warn!(
                pool = %self.pool_name,
                error = %e,
                "DRAIN failed; monitor will force-kill after grace period"
            );
        }

        if let Err(e) = self.monitor_handle.await {
            warn!(
                pool = %self.pool_name,
                error = %e,
                "monitor task did not complete cleanly"
            );
        }

        info!(pool = %self.pool_name, "worker shut down");
        Ok(())
    }
}

// ── Worker spawn ──────────────────────────────────────────────────────────────

fn spawn_worker(
    config: &WorkerConfig,
    worker_id: &str,
    pool_name: &str,
) -> Result<Child, WorkerError> {
    // Inherit the gateway's working directory so that process.cwd() in the
    // worker resolves to the same root where sky-manifest.json lives.
    // Bun resolves tsconfig paths relative to the entry file, not cwd,
    // so @sky/* aliases work regardless of working directory.
    let mut cmd = Command::new(&config.bun_path);
    cmd.arg("run")
        .arg(&config.worker_script)
        .env("SKY_WORKER_VERSION", &config.worker_version)
        .env("SKY_WORKER_ID", worker_id)
        .env("SKY_WORKER_SECRET", hex::encode(config.auth_secret))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for (k, v) in &config.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| WorkerError::Unreachable {
        pool: pool_name.to_string(),
        reason: format!("failed to spawn worker: {e}"),
    })?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    tokio::spawn(pipe_worker_logs(stdout, worker_id.to_string(), false));
    tokio::spawn(pipe_worker_logs(stderr, worker_id.to_string(), true));
    Ok(child)
}

async fn pipe_worker_logs(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    worker_id: String,
    is_stderr: bool,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        // Try to parse as JSON first — Bun workers emit structured logs
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            forward_structured(&worker_id, json);
        } else {
            // Plain text fallback
            if is_stderr {
                tracing::error!(worker_id, message = %line, source = "worker");
            } else {
                tracing::info!(worker_id, message = %line, source = "worker");
            }
        }
    }
}

fn forward_structured(worker_id: &str, json: serde_json::Value) {
    let level = json["level"].as_str().unwrap_or("INFO");
    let message = json["fields"]["message"]
        .as_str()
        .or_else(|| json["message"].as_str())
        .unwrap_or("<no message>");
    let target = json["target"].as_str().unwrap_or("worker");

    match level {
        "ERROR" => tracing::error!(worker_id, %target, message, source = "worker"),
        "WARN" => tracing::warn!(worker_id, %target, message, source = "worker"),
        "DEBUG" => tracing::debug!(worker_id, %target, message, source = "worker"),
        _ => tracing::info!(worker_id, %target, message, source = "worker"),
    }
}

fn spawn_output_forwarder<R>(stream: R, label: &'static str, pool_name: &str)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let pool = pool_name.to_string();
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => debug!(pool = %pool, stream = label, "{}", line),
                Ok(None) => break,
                Err(e) => {
                    warn!(pool = %pool, stream = label, error = %e, "output read error");
                    break;
                }
            }
        }
    });
}

// ── Readiness ─────────────────────────────────────────────────────────────────

/// Wait for the worker to connect and become healthy.
///
/// Races three outcomes: readiness timeout, child exit, worker connection.
async fn wait_for_ready(
    config: &WorkerConfig,
    pool_name: &str,
    child: &mut Child,
    listener: &SkyListener,
    shutdown_token: CancellationToken,
) -> Result<Arc<WorkerSocket>, WorkerError> {
    let deadline = Instant::now() + config.readiness_timeout;

    loop {
        if Instant::now() >= deadline {
            return Err(WorkerError::ReadinessTimeout {
                pool: pool_name.to_string(),
                timeout_ms: config.readiness_timeout.as_millis() as u64,
            });
        }

        if let Ok(Some(status)) = child.try_wait() {
            return Err(WorkerError::Unreachable {
                pool: pool_name.to_string(),
                reason: format!(
                    "worker exited during startup with status {:?}",
                    status.code()
                ),
            });
        }

        // Try to accept a connection within one poll interval.
        match tokio::time::timeout(config.poll_interval, listener.accept(&config.auth_secret)).await
        {
            Ok(Ok(socket)) => {
                // Worker connected — verify it's alive with a PING.
                let ping_timeout = config.poll_interval * 5;
                match socket.ping(pool_name, ping_timeout).await {
                    Ok(()) => {
                        info!(pool = %pool_name, "worker connected and healthy");

                        // Spawn a background health-check task that pings every 15 seconds.
                        // If a ping fails the worker is considered dead and the socket is closed,
                        // which triggers the supervisor's reconnect logic.
                        let health_socket = socket.clone();
                        let health_pool = pool_name.to_string();

                        tokio::spawn(async move {
                            let mut interval = tokio::time::interval(Duration::from_secs(15));
                            let mut consecutive_failures = 0u32;
                            const MAX_FAILURES: u32 = 3;
                            const IDLE_THRESHOLD_MS: u64 = 30_000;
                            interval.tick().await;

                            loop {
                                tokio::select! {
                                    _ = interval.tick() => {
                                        let state = health_socket.health_state();
                                        let recently_active = state.last_frame_ms_ago < IDLE_THRESHOLD_MS
                                            || state.inflight > 0;

                                        if recently_active {
                                            consecutive_failures = 0;
                                            debug!(
                                                pool              = %health_pool,
                                                inflight          = state.inflight,
                                                last_frame_ms_ago = state.last_frame_ms_ago,
                                                "health check OK (active)"
                                            );
                                            continue;
                                        }

                                        match health_socket.ping(&health_pool, Duration::from_secs(5)).await {
                                            Ok(()) => {
                                                consecutive_failures = 0;
                                                debug!(pool = %health_pool, "health check OK (idle ping)");
                                            }
                                            Err(e) => {
                                                consecutive_failures += 1;
                                                warn!(
                                                    pool              = %health_pool,
                                                    error             = %e,
                                                    failures          = consecutive_failures,
                                                    last_frame_ms_ago = state.last_frame_ms_ago,
                                                    inflight          = state.inflight,
                                                    "health check failed"
                                                );

                                                if consecutive_failures >= MAX_FAILURES {
                                                    warn!(pool = %health_pool, "worker declared dead — triggering restart");
                                                    let _ = health_socket.drain().await;
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    _ = shutdown_token.cancelled() => {
                                        // Commanded shutdown — exit quietly, supervisor handles drain
                                        debug!(pool = %health_pool, "health check stopping — supervisor shutting down");
                                        break;
                                    }
                                }
                            }
                        });

                        return Ok(socket);
                    }
                    Err(e) => {
                        debug!(pool = %pool_name, error = %e, "PING failed after connection");
                    }
                }
            }
            Ok(Err(e)) => {
                debug!(pool = %pool_name, error = %e, "accept error; retrying");
            }
            Err(_timeout) => {
                debug!(pool = %pool_name, "no worker connection yet; retrying");
            }
        }
    }
}

async fn spawn_and_ready(
    config: &WorkerConfig,
    worker_id: &str,
    pool_name: &str,
    listener: &SkyListener,
    shutdown_token: CancellationToken,
) -> Result<(Child, Arc<WorkerSocket>), WorkerError> {
    let mut child = spawn_worker(config, worker_id, pool_name)?;
    info!(pool = %pool_name, worker_id, "worker process spawned");

    if let Some(stdout) = child.stdout.take() {
        spawn_output_forwarder(stdout, "stdout", pool_name);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_forwarder(stderr, "stderr", pool_name);
    }

    let socket = match wait_for_ready(config, pool_name, &mut child, listener, shutdown_token).await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(e);
        }
    };

    Ok((child, socket))
}

pub struct MonitorInit {
    listener: SkyListener,
    connection: Arc<ArcSwap<WorkerSocket>>,
    cancel_token: CancellationToken,
    config: WorkerConfig,
    worker_id: String,
    pool_name: String,
}

// ── Monitor loop ──────────────────────────────────────────────────────────────

async fn monitor_loop(mut child: Child, mut restart_policy: RestartPolicy, init: MonitorInit) {
    let mut worker_started_at = Instant::now();

    loop {
        tokio::select! {
            _exit = child.wait() => {
                if init.cancel_token.is_cancelled() {
                    info!(pool = %init.pool_name, "worker exited after commanded shutdown");
                    break;
                }

                let ran_for = Instant::now() - worker_started_at;
                if ran_for >= init.config.healthy_reset_duration {
                    restart_policy.record_healthy_run();
                }

                match restart_policy.record_failure(Instant::now()) {
                    FailureOutcome::Permanent => {
                        warn!(pool = %init.pool_name, "crash-loop limit reached; giving up");
                        break;
                    }
                    FailureOutcome::Backoff(delay) => {
                        info!(pool = %init.pool_name, delay = ?delay, "restarting after backoff");
                        tokio::select! {
                            _ = sleep(delay) => {}
                            _ = init.cancel_token.cancelled() => break,
                        }

                        match spawn_and_ready(&init.config, &init.worker_id, init.pool_name.as_str(), &init.listener, init.cancel_token.clone()).await {
                            Ok((new_child, new_socket)) => {
                                init.connection.store(new_socket);
                                child = new_child;
                                worker_started_at = Instant::now();
                            }
                            Err(e) => {
                                warn!(pool = %init.pool_name, error = %e, "restart failed");
                                break;
                            }
                        }
                    }
                }
            }
            _ = init.cancel_token.cancelled() => {
                break;
            }
        }
    }

    // Grace period: let the worker exit cleanly, force-kill if needed.
    let grace = init.config.shutdown_grace + init.config.force_kill_buffer;
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(_)) | Ok(Err(_)) => info!(pool = %init.pool_name, "worker exited cleanly"),
        Err(_elapsed) => {
            let _ = child.kill().await;
            info!(pool = %init.pool_name, "grace period elapsed; force-killed worker");
        }
    }
}
