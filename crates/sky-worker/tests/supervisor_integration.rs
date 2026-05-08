//! Integration tests for the worker supervisor.
//!
//! These tests spawn real Bun workers and exercise the full boundary:
//! socket binding (via ID convention), worker startup, PING/PONG health
//! check, and shutdown.
//!
//! Requirements:
//!   - Bun installed and on PATH
//!   - Worker code available at the expected path
//!
//! Run with: `cargo test -p sky-worker --test supervisor_integration`

use sky_runtime::WorkerError;
use sky_worker::{Supervisor, WorkerConfig};
use std::path::PathBuf;
use std::time::Duration;

fn worker_script_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace layout")
        .join("worker")
        .join("src")
        .join("index.ts")
}

/// Generate a unique worker ID per test so parallel tests use separate sockets.
///
/// ID strings like `"test-{name}-{pid}-{nanos}"` produce paths under
/// `/tmp/sky/workers/` that won't collide between concurrent test runs.
fn unique_worker_id(test_name: &str) -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("test-{test_name}-{pid}-{nanos}")
}

fn test_config() -> WorkerConfig {
    // socket_path in WorkerConfig is no longer used for binding — the
    // supervisor derives the socket from the worker ID. We keep the field
    // so config deserialization stays intact; any placeholder value is fine.
    WorkerConfig::new("bun", worker_script_path(), "test-0.1.0")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_and_shutdown_happy_path() {
    let id = unique_worker_id("happy");
    let supervisor = Supervisor::start(test_config(), &id)
        .await
        .expect("supervisor should start");

    let _conn = supervisor.connection();

    supervisor
        .shutdown()
        .await
        .expect("shutdown should succeed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_timeout_on_invalid_binary() {
    let mut config = test_config();
    config.bun_path = PathBuf::from("/nonexistent/bun");

    let result = Supervisor::start(config, unique_worker_id("readiness-timeout")).await;
    assert!(matches!(result, Err(WorkerError::Unreachable { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_is_idempotent_via_drop() {
    let supervisor = Supervisor::start(test_config(), unique_worker_id("drop"))
        .await
        .expect("supervisor should start");

    drop(supervisor);

    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_completes_within_grace_period() {
    let mut config = test_config();
    config.shutdown_grace = Duration::from_secs(3);
    config.force_kill_buffer = Duration::from_secs(1);

    let supervisor = Supervisor::start(config, unique_worker_id("shutdown-timing"))
        .await
        .expect("supervisor should start");

    let start = std::time::Instant::now();
    supervisor
        .shutdown()
        .await
        .expect("shutdown should succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown took too long: {elapsed:?}"
    );
}
