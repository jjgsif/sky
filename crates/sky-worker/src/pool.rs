use crate::config::WorkerConfig;
use crate::supervisor::Supervisor;
use crate::transport::WorkerSocket;
use sky_runtime::WorkerError;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::warn;

/// Manages a pool of worker supervisors and load-balances requests across them.
///
/// Each slot in the pool is an independent [`Supervisor`] running its own Bun
/// process on a unique Unix Domain Socket. Requests are distributed via
/// round-robin. Each supervisor restarts independently on crash.
pub struct WorkerPool {
    supervisors: Vec<Supervisor>,
    cursor: AtomicUsize,
}

impl WorkerPool {
    /// Start `config.pool_size` supervisors, each with a unique worker ID.
    ///
    /// Worker IDs are bare numeric strings ("1", "2", ..., "N"), which produce
    /// socket paths like `/tmp/sky/workers/sky-worker-1.sock`. If any worker
    /// fails to reach readiness, the error is returned immediately and all
    /// previously started workers are dropped (their processes are killed via
    /// `kill_on_drop`).
    pub async fn new(config: WorkerConfig) -> Result<Self, WorkerError> {
        let pool_size = config.pool_size;
        let mut supervisors = Vec::with_capacity(pool_size);
        for i in 1..=pool_size {
            let sup = Supervisor::start(config.clone(), i.to_string()).await?;

            let diag_socket = sup.connection.load(); // strong Arc, lives as long as Supervisor
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let write_wait = diag_socket.write_lock_wait_ns.swap(0, Ordering::Relaxed);
                    let inflight_wait =
                        diag_socket.inflight_lock_wait_ns.swap(0, Ordering::Relaxed);
                    let routed = diag_socket.frames_routed.swap(0, Ordering::Relaxed);
                    let dropped = diag_socket.frames_dropped.swap(0, Ordering::Relaxed);

                    if routed > 0 || write_wait > 0 {
                        tracing::info!(
                            worker_id       = i.to_string(), // add this field if available
                            write_lock_ms   = write_wait / 1_000_000,
                            inflight_lock_ms = inflight_wait / 1_000_000,
                            frames_routed   = routed,
                            frames_dropped  = dropped,
                            "transport diagnostics"
                        );
                    }
                }
            });

            supervisors.push(sup);
        }
        Ok(Self {
            supervisors,
            cursor: AtomicUsize::new(0),
        })
    }

    /// Return the next worker connection via round-robin.
    ///
    /// Uses `Relaxed` ordering — the only requirement is that successive calls
    /// distribute across workers; there is no data dependency on the counter.
    /// `usize` overflow wraps to 0, and `0 % len` is always valid.
    pub fn acquire(&self) -> Arc<WorkerSocket> {
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % self.supervisors.len();
        self.supervisors[idx].connection()
    }

    /// Gracefully shut down all workers concurrently.
    ///
    /// Sends DRAIN to each worker in parallel and waits for all monitor tasks
    /// to complete. Errors from individual workers are logged but not bubbled —
    /// all supervisors receive a shutdown attempt regardless.
    pub async fn shutdown(self) -> Result<(), WorkerError> {
        let futs: Vec<_> = self
            .supervisors
            .into_iter()
            .map(|sup| sup.shutdown())
            .collect();
        for result in futures::future::join_all(futs).await {
            if let Err(e) = result {
                warn!(error = %e, "worker supervisor shutdown error");
            }
        }
        Ok(())
    }
}
