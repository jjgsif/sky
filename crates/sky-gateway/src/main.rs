//! Sky gateway binary.
//!
//! Reads a TOML configuration file, starts a worker supervisor, and
//! serves HTTP requests via axum. Forwards incoming HTTP requests to
//! the worker over the Connect protocol.

mod config;
mod errors;
mod http;

use crate::config::{GatewayConfig, LogFormat};
use crate::http::{build_router, AppState};
use anyhow::{Context, Result};
use clap::Parser;
use sky_worker::Supervisor;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// CLI arguments for the gateway binary.
#[derive(Debug, Parser)]
#[command(
    name = "sky-gateway",
    about = "The Sky framework HTTP gateway",
    version
)]
struct Cli {
    /// Path to the sky.toml configuration file.
    /// Defaults to ./sky.toml in the current directory.
    #[arg(short, long, default_value = "./sky.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration first — failures here should be visible before
    // any logging setup, so we print directly to stderr.
    let config = GatewayConfig::from_file(&cli.config).with_context(|| {
        format!("failed to load config from {}", cli.config.display())
    })?;

    // Now that we have the config, set up tracing with its preferences.
    init_tracing(&config)?;

    info!(
        listen = %config.listen.address,
        config_path = %cli.config.display(),
        "sky-gateway starting",
    );

    // Start the worker supervisor. If this fails, the gateway can't
    // serve requests, so we exit rather than running with 503s.
    let supervisor = Supervisor::start(config.worker.clone())
        .await
        .context("failed to start worker supervisor")?;
    let supervisor = Arc::new(supervisor);

    info!("worker supervisor ready; starting HTTP server");

    // Build the axum app.
    let state = AppState {
        supervisor: supervisor.clone(),
    };
    let app = build_router(state, config.listen.body_limit);

    // Bind the TCP listener.
    let listener = tokio::net::TcpListener::bind(config.listen.address)
        .await
        .with_context(|| format!("failed to bind {}", config.listen.address))?;

    info!(address = %config.listen.address, "listening for HTTP traffic");

    // Run the server until SIGTERM/SIGINT. Minimal shutdown for Phase 1;
    // E1-S8 adds the full graceful shutdown sequence with in-flight
    // request draining.
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    if let Err(e) = server.await {
        tracing::error!(error = %e, "server exited with error");
    }

    info!("HTTP server stopped; shutting down worker");

    // Try to extract the supervisor from the Arc to call shutdown.
    // If there are outstanding references, log and drop — the Drop
    // impl will still fire and kill the child.
    match Arc::try_unwrap(supervisor) {
        Ok(sup) => {
            if let Err(e) = sup.shutdown().await {
                tracing::warn!(error = %e, "worker shutdown returned error");
            }
        }
        Err(_) => {
            tracing::warn!("supervisor still has outstanding references; relying on Drop");
        }
    }

    info!("sky-gateway exited");
    Ok(())
}

/// Initialize the tracing subscriber based on configured format and level.
fn init_tracing(config: &GatewayConfig) -> Result<()> {
    let filter = EnvFilter::try_new(&config.logging.level)
        .or_else(|_| EnvFilter::try_new("info"))
        .context("failed to construct log filter")?;

    match config.logging.format {
        LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_target(true)
                .init();
        }
    }

    Ok(())
}

/// Returns a future that completes when a shutdown signal is received.
///
/// Listens for both SIGTERM (typical container/k8s shutdown) and
/// SIGINT (Ctrl-C in a terminal). Whichever arrives first triggers
/// graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received SIGINT; shutting down");
        }
        _ = terminate => {
            tracing::info!("received SIGTERM; shutting down");
        }
    }
}