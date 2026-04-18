# Sky

A TypeScript-declarative, Rust-runtime web framework.

Sky inverts the usual pattern: TypeScript code describes services, handlers, 
and dependencies declaratively, while a Rust gateway owns the HTTP surface, 
worker supervision, and infrastructure concerns. Workers run TypeScript 
business logic and talk to the gateway over a typed gRPC boundary.

This project is in pre-alpha. Public APIs are unstable until 1.0.

## Status

Phase 1: base Rust process (in progress).

See `designs/` for architectural documentation.