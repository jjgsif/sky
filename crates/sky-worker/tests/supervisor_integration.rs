//! Integration tests for the worker supervisor.
//!
//! These tests spawn real Bun workers and exercise the full boundary.
//! They require:
//!   - Bun installed and on PATH
//!   - The worker code built and available at the expected path
//!
//! Run with: `cargo test -p sky-worker --test supervisor_integration`
//!
//! These tests are slower than unit tests because they spawn processes.

use sky_proto::v1::GreetRequest;
use sky_runtime::{RequestId, WorkerError};
use sky_worker::{Supervisor, WorkerConfig};
use std::path::PathBuf;
use std::time::Duration;

/// Path to the worker script, relative to this test file.
///
/// The workspace layout is:
///   sky/
///     crates/sky-worker/tests/supervisor_integration.rs  (this file)
///     worker/src/index.ts                                (the target)
fn worker_script_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is the directory of the current crate's Cargo.toml
    // (i.e., sky/crates/sky-worker). We go up two levels to reach `sky/`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // sky/crates
        .and_then(|p| p.parent())
        .expect("workspace layout")
        .join("worker")
        .join("src")
        .join("index.ts")
}

/// Generate a unique socket path per test so parallel tests don't collide.
fn unique_socket_path(test_name: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from(format!(
        "/tmp/sky-test-{}-{}-{}.sock",
        test_name, pid, nanos
    ))
}

fn test_config(test_name: &str) -> WorkerConfig {
    let socket_path = unique_socket_path(test_name);
    // Use `bun` from PATH. Tests depend on Bun being installed.
    WorkerConfig::new("bun", worker_script_path(), socket_path, "test-0.1.0")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_greet_shutdown_happy_path() {
    let config = test_config("happy");
    let supervisor = Supervisor::start(config)
        .await
        .expect("supervisor should start");

    let client = supervisor.hello_client();
    let response = client
        .greet(
            GreetRequest {
                name: "Integration".to_string(),
            },
            RequestId::new(),
        )
        .await
        .expect("greet should succeed");

    assert!(
        response.message.contains("Integration"),
        "expected response to include greeted name, got: {}",
        response.message
    );

    supervisor
        .shutdown()
        .await
        .expect("shutdown should succeed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_timeout_on_invalid_binary() {
    let mut config = test_config("readiness-timeout");
    // Point at a binary that doesn't exist. Config validation will reject
    // this path before we even try to spawn.
    config.bun_path = PathBuf::from("/nonexistent/bun");

    let result = Supervisor::start(config).await;
    assert!(matches!(result, Err(WorkerError::Unreachable { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_is_idempotent_via_drop() {
    let config = test_config("drop");
    let supervisor = Supervisor::start(config)
        .await
        .expect("supervisor should start");

    // Drop the supervisor without calling shutdown. The Drop impl
    // should kill the child. This test just verifies we don't panic
    // or leak — the real assertion is that the test process exits
    // cleanly.
    drop(supervisor);

    // Give the kill a moment to propagate.
    tokio::time::sleep(Duration::from_millis(200)).await;
}
