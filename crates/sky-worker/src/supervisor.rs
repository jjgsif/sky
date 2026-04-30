/// Owns a running worker process and its communication channel.
///
/// # Lifecycle
///
/// The Supervisor is constructed via [`Supervisor::start`] and must be
/// cleaned up via [`Supervisor::shutdown`]. Dropping the Supervisor
/// without calling `shutdown` will leak the worker process — the
/// monitor task will continue running and hold the Bun child alive
/// until the Tokio runtime itself shuts down.
///
/// # Phase 1 limitations
///
/// - No automatic restart on crash (arrives in E1-S7).
/// - No continuous health polling after readiness.
/// - Single worker per supervisor (worker pools arrive in E3-S1).
use crate::client::HelloClient;
use crate::config::WorkerConfig;
use crate::restart_policy::{FailureOutcome, RestartPolicy};
use crate::transport::connect_uds;
use arc_swap::ArcSwap;
use sky_proto::v1::{
    HealthRequest, HealthStatus, ShutdownRequest, worker_control_client::WorkerControlClient,
};
use sky_runtime::WorkerError;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

/// Owns a running worker process and its communication channel.
///
/// Construct via [`Supervisor::start`], use via [`Supervisor::hello_client`],
/// clean up via [`Supervisor::shutdown`] (or drop, as a fallback).
pub struct Supervisor {
    pub channel: Arc<ArcSwap<Channel>>,
    config: WorkerConfig,
    pool_name: String,
    shutdown_token: CancellationToken,
    monitor_handle: JoinHandle<()>,
}

impl Supervisor {
    /// Start a worker and wait for it to become healthy.
    ///
    /// Spawns the worker process, streams its output to tracing, connects
    /// to its Unix socket, and polls `WorkerControl.Health` until the
    /// worker reports READY. Returns only after the worker is fully ready
    /// to accept RPCs.
    ///
    /// # Errors
    ///
    /// - [`WorkerError::Unreachable`] if the process fails to spawn.
    /// - [`WorkerError::ReadinessTimeout`] if the worker doesn't become
    ///   healthy within the configured timeout.
    /// - [`sky_runtime::ConfigError`] (via `WorkerError::Unreachable`) if
    ///   config validation fails.
    pub async fn start(config: WorkerConfig) -> Result<Self, WorkerError> {
        let pool_name = "default".to_string();

        // Validate config before spawning anything.
        config.validate().map_err(|e| WorkerError::Unreachable {
            pool: pool_name.clone(),
            reason: format!("invalid config: {e}"),
        })?;

        // Spawn the Bun process.
        let (child, channel) = spawn_and_ready(&config, &pool_name).await?;
        let channel_swap: Arc<ArcSwap<Channel>> = Arc::new(ArcSwap::from_pointee(channel.clone())); // Arc<ArcSwap<Channel>>
        let restart_policy = RestartPolicy::new(&config);

        let shutdown_token = CancellationToken::new();

        let monitor_handle = tokio::spawn({
            let shutdown_token = shutdown_token.clone();
            let config = config.clone();
            let pool_name = pool_name.clone();
            let channel = channel_swap.clone();

            async move {
                monitor_loop(
                    child,
                    channel,
                    shutdown_token,
                    config,
                    restart_policy,
                    &pool_name,
                )
                .await;
            }
        });

        Ok(Self {
            channel: channel_swap,
            config,
            pool_name,
            shutdown_token,
            monitor_handle,
        })
    }

    /// Return a client for calling HelloService on this worker.
    #[must_use]
    pub fn hello_client(&self) -> HelloClient {
        let channel = self.channel.load_full();
        HelloClient::new((*channel).clone(), &self.pool_name)
    }

    /// Shutdown the worker gracefully. Consumes self.
    pub async fn shutdown(self) -> Result<(), WorkerError> {
        info!(pool = %self.pool_name, "initiating worker shutdown");

        // Step 1: Cancel the token. After this, the monitor will not try
        // to restart the worker if it dies.
        self.shutdown_token.cancel();

        // Step 2: Send the graceful Shutdown RPC. Best-effort — if it fails,
        // we'll let the monitor's timeout path handle cleanup.
        let current_channel = self.channel.load_full();
        let shutdown_result = send_shutdown(&current_channel, &self.config).await;
        if let Err(e) = shutdown_result {
            warn!(
                pool = %self.pool_name,
                error = %e,
                "shutdown RPC failed; monitor will force-kill"
            );
        }

        // Step 3: Wait for the monitor task to complete. The monitor is
        // responsible for waiting on the child, force-killing on timeout,
        // and reaping. When its task future completes, we know cleanup is done.
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

fn spawn_worker(config: &WorkerConfig, pool_name: &str) -> Result<Child, WorkerError> {
    let child = Command::new(&config.bun_path)
        .arg("run")
        .arg(&config.worker_script)
        .env("SKY_WORKER_VERSION", &config.worker_version)
        .env("SKY_WORKER_ID", "1")
        // Pipe stdout/stderr so we can forward them to tracing.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Kill the child if the parent dies unexpectedly. Prevents
        // orphaned workers on supervisor crash.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| WorkerError::Unreachable {
            pool: pool_name.to_string(),
            reason: format!("failed to spawn worker process: {e}"),
        })?;

    Ok(child)
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
                Ok(Some(line)) => {
                    debug!(pool = %pool, stream = label, "{}", line);
                }
                Ok(None) => break, // EOF — child closed this stream
                Err(e) => {
                    warn!(pool = %pool, stream = label, error = %e, "error reading output");
                    break;
                }
            }
        }
    });
}

async fn try_health_check(
    socket_path: &std::path::Path,
) -> Result<Channel, Box<dyn std::error::Error + Send + Sync>> {
    let channel = connect_uds(socket_path.to_path_buf()).await?;
    let mut client = WorkerControlClient::new(channel.clone());

    let response = client.health(HealthRequest {}).await?;
    let health = response.into_inner();

    match HealthStatus::try_from(health.status) {
        Ok(HealthStatus::Ready) => Ok(channel),
        Ok(other) => Err(format!("worker reported status: {:?}", other).into()),
        Err(_) => Err(format!("worker reported unknown status code: {}", health.status).into()),
    }
}

async fn send_shutdown(channel: &Channel, config: &WorkerConfig) -> Result<(), WorkerError> {
    let mut client = WorkerControlClient::new(channel.clone());
    let request = ShutdownRequest {
        grace_period_seconds: config.shutdown_grace.as_secs() as u32,
    };

    client
        .shutdown(request)
        .await
        .map_err(|status| WorkerError::WorkerReturnedError {
            pool: "default".to_string(),
            message: format!("shutdown RPC failed: {}", status.message()),
        })?;

    Ok(())
}

async fn wait_for_ready(
    config: &WorkerConfig,
    pool_name: &str,
    child: &mut Child,
) -> Result<Channel, WorkerError> {
    let deadline = Instant::now() + config.readiness_timeout;

    loop {
        // Check whether we've exceeded the readiness timeout.
        if Instant::now() >= deadline {
            return Err(WorkerError::ReadinessTimeout {
                pool: pool_name.to_string(),
                timeout_ms: config.readiness_timeout.as_millis() as u64,
            });
        }

        // Check whether the child exited while we were waiting.
        // This catches crashes during startup (e.g., missing dependencies,
        // port in use, etc.).
        if let Ok(Some(status)) = child.try_wait() {
            return Err(WorkerError::Unreachable {
                pool: pool_name.to_string(),
                reason: format!(
                    "worker exited during startup with status {:?}",
                    status.code()
                ),
            });
        }

        // Try to connect and health-check.
        match try_health_check(Path::new("/tmp/sky/workers/sky-worker-1.sock")).await {
            Ok(channel) => return Ok(channel),
            Err(e) => {
                debug!(
                    pool = %pool_name,
                    error = %e,
                    "worker not yet ready; retrying",
                );
                sleep(config.poll_interval).await;
            }
        }
    }
}

async fn spawn_and_ready(
    config: &WorkerConfig,
    pool_name: &str,
) -> Result<(Child, Channel), WorkerError> {
    let mut child = spawn_worker(config, pool_name)?;
    info!("Worker spawned");

    if let Some(stdout) = child.stdout.take() {
        spawn_output_forwarder(stdout, "worker stdout", pool_name);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_forwarder(stderr, "worker stderr", pool_name);
    }

    let channel = match wait_for_ready(config, pool_name, &mut child).await {
        Ok(c) => c,
        Err(e) => {
            // Kill the child before returning — otherwise the caller doesn't
            // know to clean up, and we leak a process.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(e);
        }
    };

    Ok((child, channel))
}

async fn monitor_loop(
    mut child: Child,
    channel: Arc<ArcSwap<Channel>>,
    cancel_token: CancellationToken,
    config: WorkerConfig,
    mut restart_policy: RestartPolicy,
    pool_name: &str,
) {
    let worker_started_at = Instant::now();

    loop {
        tokio::select! {
            _exit_result = child.wait() => {
                if cancel_token.is_cancelled() {
                    info!("Child has exited due to shutdown");
                    break;
                }

                                let ran_for = Instant::now() - worker_started_at;
                if ran_for >= config.healthy_reset_duration {
                    restart_policy.record_healthy_run();
                }

                match restart_policy.record_failure(Instant::now()) {
                    FailureOutcome::Permanent => {
                        info!("Unable to spawn and ready worker within {:?}", restart_policy.max_backoff);
                        break;
                    }

                    FailureOutcome::Backoff(delay) => {
                        info!("Restarting after backoff delay: {:?}", delay);
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {
                            }
                            _ = cancel_token.cancelled() => break,
                        }

                        match spawn_and_ready(&config, pool_name).await {
                        Ok((new_child, new_channel)) => {
                            channel.store(Arc::new(new_channel));
                            child = new_child;
                        }
                        Err(_e) => {
                            info!("Unable to spawn worker after delay");
                            break;
                        }
                    }
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                break;
            }
        }
    }

    let grace = config.shutdown_grace + config.force_kill_buffer;
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(_status)) => {
            info!("Worker exited")
        }
        Ok(Err(_e)) => {
            info!("Worker exited")
        }
        Err(_elapsed) => {
            let _ = child.kill().await;
            info!("Grace period reached - killing worker");
        }
    }
}
