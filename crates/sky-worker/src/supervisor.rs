//! The per-worker supervisor.
//!
//! A [`Supervisor`] owns a single worker process from spawn to exit.
//! It ensures the worker is alive and healthy before exposing a client,
//! and coordinates graceful shutdown when dropped or explicitly closed.
//!
//! # Phase 1 limitations
//!
//! - No automatic restart on crash (arrives in E1-S7).
//! - No continuous health polling after readiness.
//! - Single worker per supervisor (worker pools arrive in E3-S1).

use crate::client::HelloClient;
use crate::config::WorkerConfig;
use crate::transport::connect_uds;
use sky_proto::v1::{
    worker_control_client::WorkerControlClient, HealthRequest, HealthStatus,
    ShutdownRequest,
};
use sky_runtime::WorkerError;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout, Instant};
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

/// Owns a running worker process and its communication channel.
///
/// Construct via [`Supervisor::start`], use via [`Supervisor::hello_client`],
/// clean up via [`Supervisor::shutdown`] (or drop, as a fallback).
pub struct Supervisor {
    child: Option<Child>,
    channel: Channel,
    config: WorkerConfig,
    pool_name: String,
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

        info!(
            socket_path = %config.socket_path.display(),
            worker_version = %config.worker_version,
            "spawning worker",
        );

        // Spawn the Bun process.
        let mut child = spawn_worker(&config, &pool_name)?;

        // Stream stdout/stderr to our tracing output via background tasks.
        // These tasks run until the child's streams close (on exit).
        if let Some(stdout) = child.stdout.take() {
            spawn_output_forwarder(stdout, "worker stdout", &pool_name);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_output_forwarder(stderr, "worker stderr", &pool_name);
        }

        // Wait for the worker to become healthy, respecting the timeout.
        // This also detects early process exit (crash during startup).
        let channel = wait_for_ready(&config, &pool_name, &mut child).await?;

        info!(pool = %pool_name, "worker ready");

        Ok(Self {
            child: Some(child),
            channel,
            config,
            pool_name,
        })
    }

    /// Return a client for calling HelloService on this worker.
    #[must_use]
    pub fn hello_client(&self) -> HelloClient {
        HelloClient::new(self.channel.clone(), &self.pool_name)
    }

    /// Shutdown the worker gracefully. Consumes self.
    pub async fn shutdown(mut self) -> Result<(), WorkerError> {
        self.shutdown_inner().await
    }

    async fn shutdown_inner(&mut self) -> Result<(), WorkerError> {
        // Take the child out so it's owned here. After this, the Supervisor
        // no longer has a live child to drop.
        let Some(mut child) = self.child.take() else {
            // Already shut down or never had one. Nothing to do.
            return Ok(());
        };

        info!(pool = %self.pool_name, "initiating worker shutdown");

        // Send Shutdown RPC to the worker.
        let shutdown_result = send_shutdown(&self.channel, &self.config).await;
        if let Err(e) = shutdown_result {
            warn!(pool = %self.pool_name, error = %e, "shutdown RPC failed; will force-kill");
        }

        // Wait for the child to exit within the grace + buffer window.
        let total_wait = self.config.shutdown_grace + self.config.force_kill_buffer;
        let exit_result = timeout(total_wait, child.wait()).await;

        match exit_result {
            Ok(Ok(status)) => {
                info!(
                    pool = %self.pool_name,
                    exit_code = ?status.code(),
                    "worker exited cleanly",
                );
                Ok(())
            }
            Ok(Err(e)) => {
                error!(pool = %self.pool_name, error = %e, "error awaiting worker exit");
                Err(WorkerError::Unreachable {
                    pool: self.pool_name.clone(),
                    reason: format!("wait failed: {e}"),
                })
            }
            Err(_timeout_elapsed) => {
                warn!(
                    pool = %self.pool_name,
                    "worker did not exit within grace window; killing",
                );
                // `kill` sends SIGKILL on Unix.
                let _ = child.kill().await;
                // Reap the child so it doesn't become a zombie.
                let _ = child.wait().await;
                Ok(())
            }
        }
    }
}

fn spawn_worker(config: &WorkerConfig, pool_name: &str) -> Result<Child, WorkerError> {
    let child = Command::new(&config.bun_path)
        .arg("run")
        .arg(&config.worker_script)
        .env("SKY_WORKER_SOCKET", &config.socket_path)
        .env("SKY_WORKER_VERSION", &config.worker_version)
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
                reason: format!("worker exited during startup with status {:?}", status.code()),
            });
        }

        // Try to connect and health-check.
        match try_health_check(&config.socket_path).await {
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

impl Drop for Supervisor {
    /// Attempts to clean up the worker if the Supervisor is dropped
    /// without explicit shutdown.
    ///
    /// This is a fallback — preferring `shutdown().await` is better
    /// because Drop can't run async code, so cleanup here is best-effort.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            warn!(
                pool = %self.pool_name,
                "Supervisor dropped without explicit shutdown; killing child",
            );
            // Synchronous kill via the inner std process handle.
            // We can't await here, so we can't do graceful shutdown.
            // kill_on_drop on the Command will also fire, but we do
            // an explicit start_kill for clarity.
            let _ = child.start_kill();
        }
    }
}