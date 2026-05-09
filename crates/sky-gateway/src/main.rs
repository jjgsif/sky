//! Sky gateway binary.
//!
//! Reads a TOML configuration file, starts a worker supervisor, and
//! serves HTTP requests via axum. Forwards incoming HTTP requests to
//! the worker over the Sky framing protocol.

mod config;
mod cors;
mod manifest;
mod validation;
mod router;
mod rate_limit;

use crate::config::{GatewayConfig, LogFormat};
use crate::cors::CorsRegistry;
use crate::manifest::Manifest;
use crate::router::{RouterState, build_manifest_router};
use crate::validation::SchemaRegistry;
use anyhow::{Context, Result};
use clap::Parser;
use futures::future::join_all;
use sky_worker::WorkerPool;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;
use crate::rate_limit::{connect_redis, RateLimiter};

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
    let config = GatewayConfig::from_file(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    let _manifest = Manifest::from_file(&config.manifest_path)
        .with_context(|| format!("failed to load config from {}", &config.manifest_path.display()))?;

    // Now that we have the config, set up tracing with its preferences.
    init_tracing(&config)?;

    info!(
        listen = %config.listen.address,
        config_path = %cli.config.display(),
        "sky-gateway starting",
    );

    // Start worker pool. If any worker fails readiness, exit immediately.
    let pool = WorkerPool::new(config.worker.clone())
        .await
        .context("failed to start worker pool")?;
    let pool = Arc::new(pool);

    info!(pool_size = config.worker.pool_size, "worker pool ready; starting HTTP server");

    let manifest = Arc::new(
        Manifest::from_file(&config.manifest_path).expect("failed to load manifest"),
    );
    let schema_registry = Arc::new(
        SchemaRegistry::from_manifest(&manifest).expect("failed to compile schemas"),
    );

    let cors_registry = Arc::new(CorsRegistry::from_manifest(&manifest));

    let rate_limit_registry = Arc::new(match &config.rate_limit.redis_url {
        Some(url) => {
            match connect_redis(url, config.rate_limit.pool_size, config.rate_limit.command_timeout).await {
                Ok(redis) => {
                    info!(url = %url, pool_size = config.rate_limit.pool_size, "rate-limit Redis backend connected");
                    RateLimiter::with_redis_backend(&manifest, redis)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        url = %url,
                        "failed to connect rate-limit Redis backend; falling back to local window"
                    );
                    RateLimiter::from_manifest(&manifest)
                }
            }
        }
        None => RateLimiter::from_manifest(&manifest),
    });
    
    let app = build_manifest_router(RouterState {
        manifest: manifest.clone(),
        schema_registry,
        cors: cors_registry,
        rate_limit: rate_limit_registry,
        pool: Some(pool.clone()),
    });

    let drain_timeout = config.listen.drain_timeout;
    let threads = if config.listen.accept_threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        config.listen.accept_threads
    };

    info!(
        threads,
        address = %config.listen.address,
        "binding accept pool with SO_REUSEPORT"
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let mut serve_handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let listener = make_listener(config.listen.address)
            .with_context(|| format!("failed to bind {}", config.listen.address))?;
        let app = app.clone();
        let mut rx = shutdown_rx.clone();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.wait_for(|&v| v).await;
                    info!(
                        drain_timeout_secs = drain_timeout.as_secs(),
                        "shutdown signal received; draining in-flight requests"
                    );
                })
                .await
        });
        serve_handles.push(handle);
    }

    // Race: all accept workers drain vs. drain_timeout elapses.
    tokio::select! {
        results = join_all(serve_handles) => {
            for result in results {
                if let Ok(Err(e)) = result {
                    tracing::error!(error = %e, "accept worker exited with error");
                }
            }
            info!("HTTP connections drained");
        }
        _ = async {
            let mut rx = shutdown_rx;
            let _ = rx.wait_for(|&v| v).await;
            tokio::time::sleep(drain_timeout).await;
        } => {
            info!(
                drain_timeout_secs = drain_timeout.as_secs(),
                "drain timeout elapsed; forcing connection closure"
            );
        }
    }

    info!("HTTP server stopped; shutting down worker");

    match Arc::try_unwrap(pool) {
        Ok(p) => {
            if let Err(e) = p.shutdown().await {
                tracing::warn!(error = %e, "worker pool shutdown returned error");
            }
        }
        Err(_) => {
            tracing::warn!("worker pool still has outstanding references; relying on Drop");
        }
    }

    info!("sky-gateway exited");
    Ok(())
}


/// Bind a TCP socket with SO_REUSEPORT and SO_REUSEADDR set.
///
/// SO_REUSEPORT lets N sockets share the same addr:port. The kernel distributes
/// incoming connections across them, eliminating the single accept-queue bottleneck.
fn make_listener(addr: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let std_listener: std::net::TcpListener = socket.into();
    Ok(tokio::net::TcpListener::from_std(std_listener)?)
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
