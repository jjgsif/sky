//! Worker supervisor and Connect client for the Sky framework.
//!
//! This crate owns the lifecycle of TypeScript workers: spawning Bun
//! processes, health-checking them over the Connect boundary, and
//! dispatching RPCs to them via typed client wrappers.
//!
//! # Architecture
//!
//! The primary type is [`Supervisor`], which owns a single worker
//! process throughout its lifetime. It is constructed asynchronously
//! via [`Supervisor::start`], which spawns the worker and waits for
//! it to become healthy.
//!
//! Once started, the supervisor exposes typed clients (e.g., [`HelloClient`])
//! for calling the worker's RPC services. Clients are cheap to clone
//! and may be used concurrently from multiple tasks.
//!
//! # Phase 1 limitations
//!
//! - Only one worker per supervisor (no pool).
//! - No automatic restart on worker crash (arrives in E1-S7).
//! - Only HelloService exposed via typed client (other services arrive
//!   with the manifest layer in Phase 2).

mod client;
mod config;
mod restart_policy;
mod supervisor;
mod transport;

pub use client::HelloClient;
pub use config::WorkerConfig;
pub use supervisor::Supervisor;

#[doc(hidden)]
pub use transport::connect_uds as connect_uds_for_probe;
