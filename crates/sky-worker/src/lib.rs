//! Worker supervisor and Sky framing transport for the Sky framework.
//!
//! This crate owns the lifecycle of TypeScript workers: spawning Bun
//! processes, waiting for them to connect over the Sky framing protocol,
//! health-checking via PING/PONG, and dispatching requests via INVOKE
//! frames.
//!
//! # Architecture
//!
//! The gateway binds a Unix Domain Socket ([`SkyListener`]) before
//! spawning the worker. The worker connects as a client. The resulting
//! [`WorkerSocket`] is the multiplexed channel for all in-flight requests.
//!
//! The primary type is [`Supervisor`], which owns a single worker process.
//! Construct via [`Supervisor::start`], dispatch via [`Supervisor::connection`],
//! clean up via [`Supervisor::shutdown`].

mod config;
mod pool;
mod restart_policy;
mod supervisor;
pub mod transport;

pub use config::WorkerConfig;
pub use pool::WorkerPool;
pub use supervisor::{socket_path_for_id, Supervisor};
pub use transport::{InboundFrame, InvokePayload, PendingRequest, SkyListener, WorkerSocket};
