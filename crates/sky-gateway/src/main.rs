//! Sky gateway binary.
//!
//! Reads a TOML configuration file, starts a worker supervisor, and
//! serves HTTP requests via axum. Forwards incoming HTTP requests to
//! the worker over the Sky framing protocol.

mod auth;
mod config;
mod cors;
mod manifest;
mod proxy;
mod rate_limit;
mod router;
mod validation;

use crate::auth::AuthValidator;
use crate::config::{GatewayConfig, LogFormat};
use crate::cors::CorsRegistry;
use crate::manifest::Manifest;
use crate::rate_limit::{RateLimiter, connect_redis};
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

    /// Run in development mode.
    ///
    /// When set, requests under `[frontend].prefix` are proxied to
    /// `[frontend].dev_server` instead of being served from the built output
    /// directory. Intended to be set by `sky dev`; not for production use.
    #[arg(long)]
    dev: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration first — failures here should be visible before
    // any logging setup, so we print directly to stderr.
    let config = GatewayConfig::from_file(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    let _manifest = Manifest::from_file(&config.manifest_path).with_context(|| {
        format!(
            "failed to load config from {}",
            &config.manifest_path.display()
        )
    })?;

    // Now that we have the config, set up tracing with its preferences.
    init_tracing(&config)?;

    info!(
        listen = %config.listen.address,
        config_path = %cli.config.display(),
        "sky-gateway starting",
    );

    // In production mode, ensure the compiled frontend assets exist and are
    // not stale relative to the declared source directories. Build first if needed.
    if !cli.dev
        && let Some(frontend) = &config.frontend
    {
        maybe_build_frontend(frontend).await?;
    }

    // Start worker pool. Propagate auth secret to workers if configured.
    let mut worker_config = config.worker.clone();
    if !config.auth.jwt_secret.is_empty() {
        worker_config.env.insert(
            "SKY_AUTH_SECRET".to_string(),
            config.auth.jwt_secret.clone(),
        );
    }
    let pool = WorkerPool::new(worker_config)
        .await
        .context("failed to start worker pool")?;
    let pool = Arc::new(pool);

    if cli.dev {
        info!("running in development mode");
    }

    info!(
        pool_size = config.worker.pool_size,
        "worker pool ready; starting HTTP server"
    );

    let manifest =
        Arc::new(Manifest::from_file(&config.manifest_path).expect("failed to load manifest"));
    let schema_registry =
        Arc::new(SchemaRegistry::from_manifest(&manifest).expect("failed to compile schemas"));

    let cors_registry = Arc::new(CorsRegistry::from_manifest(&manifest));

    let auth_validator = Arc::new(
        AuthValidator::from_manifest(&manifest, &config.auth.jwt_secret)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );

    let rate_limit_registry = Arc::new(match &config.rate_limit.redis_url {
        Some(url) => {
            match connect_redis(
                url,
                config.rate_limit.pool_size,
                config.rate_limit.command_timeout,
            )
            .await
            {
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

    let app = build_manifest_router(
        RouterState {
            manifest: manifest.clone(),
            schema_registry,
            cors: cors_registry,
            auth: auth_validator,
            rate_limit: rate_limit_registry,
            pool: Some(pool.clone()),
            body_limit: config.listen.body_limit as usize,
            upload_limit: config.listen.upload_limit as usize,
            frontend_prefix: None, // populated by build_manifest_router
        },
        config.static_files.as_ref(),
        config.frontend.as_ref(),
        cli.dev,
    );

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
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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

/// Check whether the compiled frontend output is absent or stale and, if so,
/// run `cfg.build` before the gateway accepts any traffic.
///
/// Staleness is determined by comparing the mtime of `output/index.html`
/// (the sentinel written by every Vite build) against the most-recently
/// modified file found under `cfg.sources`. If `sources` is empty the build
/// is only triggered when the output sentinel is missing.
async fn maybe_build_frontend(cfg: &crate::config::FrontendConfig) -> Result<()> {
    let sentinel = cfg.output.join("index.html");

    let needs_build = if !sentinel.exists() {
        info!(
            output = %cfg.output.display(),
            "frontend output absent or missing index.html; triggering build"
        );
        true
    } else if !cfg.sources.is_empty() {
        let build_time = sentinel.metadata()?.modified()?;
        match latest_mtime_in_dirs(&cfg.sources)? {
            Some(src_mtime) if src_mtime > build_time => {
                info!(
                    output = %cfg.output.display(),
                    "frontend source newer than last build; triggering build"
                );
                true
            }
            _ => false,
        }
    } else {
        false
    };

    if needs_build {
        run_build_command(&cfg.build).await?;
    } else {
        info!(output = %cfg.output.display(), "frontend output is up to date; skipping build");
    }

    Ok(())
}

/// Walk `dirs` recursively and return the most-recent file mtime found.
fn latest_mtime_in_dirs(dirs: &[std::path::PathBuf]) -> Result<Option<std::time::SystemTime>> {
    use std::time::SystemTime;

    fn walk(path: &std::path::Path, latest: &mut Option<SystemTime>) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(&entry.path(), latest)?;
            } else if ft.is_file()
                && let Ok(mtime) = entry.metadata()?.modified()
            {
                *latest = Some(match *latest {
                    Some(l) if l >= mtime => l,
                    _ => mtime,
                });
            }
        }
        Ok(())
    }

    let mut latest: Option<SystemTime> = None;
    for dir in dirs {
        if dir.is_dir() {
            walk(dir, &mut latest)?;
        }
    }
    Ok(latest)
}

/// Run a shell command via `sh -c` and fail if it exits non-zero.
async fn run_build_command(cmd: &str) -> Result<()> {
    info!(command = %cmd, "running frontend build");
    let status = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .await
        .context("failed to spawn frontend build command")?;
    if !status.success() {
        anyhow::bail!(
            "frontend build exited with status {:?}; refusing to start with stale assets",
            status.code()
        );
    }
    info!("frontend build completed successfully");
    Ok(())
}

/// Bind a TCP socket with SO_REUSEPORT and SO_REUSEADDR set.
///
/// SO_REUSEPORT lets N sockets share the same addr:port. The kernel distributes
/// incoming connections across them, eliminating the single accept-queue bottleneck.
fn make_listener(addr: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
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
