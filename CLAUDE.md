# Sky Framework — CLAUDE.md

## Working Branch Workflow

Before editing, creating, or deleting any files in this repository:

1. **Check the current branch.** If it is not `develop`, switch to `develop` first.
2. **Commit** the working tree to establish a stable fall-back point on `develop` *before* starting new changes.
3. Only then begin the requested edits.

This keeps `master` (and any feature branches) clean and guarantees there is always a known-good `develop` checkpoint to revert to if a change goes sideways. If `develop` does not exist, ask the user before creating it — never silently branch from an arbitrary commit.

## Project Overview

Sky is a TypeScript-declarative, Rust-runtime web framework. TypeScript describes services, handlers, and DI via decorators and emits a manifest. Rust owns the HTTP gateway, worker supervision, and observability. Bun workers execute business logic over the Sky framing protocol (custom binary over Unix Domain Sockets).

**Core thesis:** "Monolithic codebase in a distributed system environment." Write as a monolith, deploy as distributed. RoadRunner-for-TypeScript philosophy.

**Target audience:** Mid-size SaaS TypeScript teams at production scale (20–100 engineers). Secondary: platform teams, technical founders, PHP/RoadRunner migrants.

**Deliberate non-audience:** Greenfield prototypes, serverless/edge, max-perf specialized workloads, enterprise Java/.NET.

## Repository Layout

```
sky/
├── Cargo.toml                    # Workspace root
├── sky.toml                      # Example gateway config
├── sky-manifest.json             # Manifest emitted by `sky build` (schema source of truth)
├── crates/
│   ├── sky-runtime/              # Shared types: errors, RequestId, HandlerDescriptor
│   ├── sky-worker/               # Worker supervisor, Sky framing transport, restart policy
│   │   ├── src/
│   │   │   ├── config.rs         # WorkerConfig (serde, humantime-serde)
│   │   │   ├── supervisor.rs     # Supervisor, monitor task, spawn_and_ready
│   │   │   ├── transport.rs      # Sky framing protocol over Unix domain sockets
│   │   │   ├── restart_policy.rs # RestartPolicy state machine + FailureOutcome
│   │   │   ├── pool.rs           # WorkerPool: round-robin over N Supervisors
│   │   │   └── lib.rs
│   │   ├── tests/
│   │   │   └── supervisor_integration.rs
│   │   └── examples/
│   │       └── probe.rs          # Diagnostic tool for testing UDS connectivity
│   ├── sky-gateway/              # The HTTP gateway binary
│   │   └── src/
│   │       ├── main.rs           # Entry point: config loading, tracing, server startup
│   │       ├── config.rs         # GatewayConfig with [listen], [logging], [worker] sections
│   │       ├── manifest.rs       # Manifest loading and routing table construction
│   │       ├── router.rs         # Manifest-driven axum router, generic_handler, RouterState
│   │       ├── validation.rs     # SchemaRegistry: JSON Schema compilation from manifest
│   │       └── errors.rs         # HttpError newtype, WorkerError→HTTP status mapping
│   ├── sky-router/               # Stub — Phase 3
│   └── sky-middleware/           # Stub — Phase 3
└── worker/                       # TypeScript/Bun worker (also the `sky` npm package)
    ├── package.json              # name: "sky", exports: ./runtime, ./decorators, ./emitter
    ├── tsconfig.json
    ├── sky-manifest.json         # Generated manifest (gitignored in real projects)
    └── src/
        ├── index.ts              # Worker entry point; passes services to startServer()
        ├── decorators/           # @Service, @Handler, @Body, @Param, @Query, @Header, @Group, @ZodBody
        ├── emitter/              # Manifest assembler: type walker, JSON Schema emitter, registry
        ├── runtime/
        │   ├── server.ts         # startServer() — wires DI, dispatcher, socket, shutdown
        │   ├── dispatcher.ts     # Invocation loop: routes INVOKE frames to handler methods
        │   ├── service-registry.ts  # ServiceRegistry over ServiceMap populated by decorators
        │   ├── logger.ts         # Pino structured logger
        │   ├── response.ts       # Response<T> wrapper for explicit status/headers/cookies
        │   └── services/
        │       └── hello.ts      # HelloService — framework smoke-test handler
        └── transport/
            ├── socket.ts         # SkyWorkerSocket: connects to gateway UDS, sends frames
            ├── types.ts          # Frame type constants, SkyInvocation interface
            ├── util.ts           # MessagePack encode/decode helpers
            └── index.ts
```

## Architecture & Key Decisions

### Boundary Protocol — Sky Framing

The gateway and worker communicate over a **custom binary framing protocol** on a Unix Domain Socket. There is no gRPC, Connect, or HTTP/2 between them.

**Frame format** — 10-byte header followed by payload:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 1 | version (always `0x01`) |
| 1 | 1 | frame_type |
| 2 | 4 | request_id (u32 big-endian) |
| 6 | 4 | payload_len (u32 big-endian) |
| 10 | N | payload bytes |

**Frame types:**

| Hex | Name | Direction | Payload encoding |
|-----|------|-----------|------------------|
| 0x01 | INVOKE | Gateway → Worker | MessagePack |
| 0x02 | RESPONSE_HEAD | Worker → Gateway | MessagePack |
| 0x03 | RESPONSE_CHUNK | Worker → Gateway | Raw bytes |
| 0x04 | RESPONSE_END | Worker → Gateway | Empty |
| 0x05 | ERROR | Worker → Gateway | MessagePack |
| 0x06 | PING | Gateway → Worker | Empty |
| 0x07 | PONG | Worker → Gateway | Empty |
| 0x08 | DRAIN | Gateway → Worker | Empty |

**INVOKE payload** (MessagePack):
```
{ handler_id, method, path, params, query, headers, body: bytes }
```
`body` uses `serde_bytes` / `#[serde(with = "serde_bytes")]` for correct binary encoding.

**RESPONSE_HEAD payload** (MessagePack):
```
{ status: u16, headers: { string: string } }
```

**ERROR payload** (MessagePack):
```
{ code: string, message: string, stack?: string }
```

### Socket Ownership and Worker ID Convention

- **Gateway (Rust) creates and binds the socket.** The worker connects as a client. The gateway never deletes a socket it didn't create.
- Socket path formula (both sides derive it identically):
  ```
  /tmp/sky/workers/sky-worker-{SKY_WORKER_ID}.sock
  ```
- `SKY_WORKER_ID` is set as an environment variable by the Rust supervisor when spawning the Bun process. The worker reads it to determine its socket path.
- `SkyListener::bind()` removes any stale socket file and creates parent directories before binding.
- The listener stays bound across worker restarts so a crashed worker can reconnect without rebinding.

### Multiplexing

A single socket carries many concurrent requests, each identified by a `request_id` (u32). The gateway's `WorkerSocket` maintains an in-flight map (`HashMap<u32, mpsc::UnboundedSender<InboundFrame>>`). The background `read_loop` task parses incoming frames and routes each to the correct sender by `request_id`.

### Health and Shutdown

- **PING/PONG**: Health check. The supervisor sends PING after connection and awaits PONG within a timeout. Used both at initial readiness and during restart detection.
- **DRAIN**: Graceful shutdown signal. The gateway sends DRAIN to tell the worker to finish in-flight requests and exit. The worker calls `close()` after the grace period.

### Manifest and Routing

The gateway is **manifest-driven**. At startup it loads `sky-manifest.json` (path from `manifest_path` in `sky.toml`) and builds the routing table from it. Each route entry carries:
- `handler_id`: `"ClassName.methodName"` — forwarded as-is in the INVOKE frame
- `default_status`: from the manifest (emitter defaults: GET→200, POST→201, PUT→200, DELETE→204)
- `has_body`: whether to read the request body before forwarding
- The JSON Schema for request body validation (compiled by `SchemaRegistry`)

The TS worker's `dispatcher.ts` mirrors this: it reads the same manifest to look up the `status` and `validate` flag for each handler, then dispatches to the DI-resolved service method.

**Important:** `dispatcher.ts` loads the manifest via top-level `await import(\`${process.cwd()}/sky-manifest.json\`)`. The supervisor does **not** override `current_dir` when spawning the worker, so `process.cwd()` in the worker equals the gateway's working directory — the same directory that `manifest_path` in `sky.toml` is relative to.

### Config Architecture
- `sky.toml` with `version = "1"` for forward compatibility
- Sections: `[listen]`, `[logging]`, `[worker]`
- `manifest_path` defaults to `./sky-manifest.json`; resolved relative to the gateway's working directory
- Default config path: `./sky.toml`, overridable via `--config`
- `WorkerConfig` does **not** have a `socket_path` field — the socket path is always derived from `SKY_WORKER_ID`

### Worker Supervision
- Single Rust binary configurable into deployment modes (monolith, worker-pool, distributed — future)
- `Supervisor::start(config, worker_id)` — `worker_id` is the sole source of truth for the socket path
- Monitor task (`tokio::spawn`) owns the child process and restart policy
- `CancellationToken` distinguishes commanded shutdown from unexpected exit
- `Arc<ArcSwap<WorkerSocket>>` enables atomic connection replacement on worker restart without blocking in-flight RPCs
- Supervisor does NOT implement `Drop`; `shutdown()` must be called explicitly
- `spawn_and_ready()` is the extracted helper for spawning + health-checking a worker

### Restart Policy
- Exponential backoff: initial (100ms) → doubling → capped at max (30s)
- Rolling window failure tracking: 10 failures within 5 minutes triggers permanent failure
- `record_healthy_run()` resets after `healthy_reset_duration` (60s) of stable running
- Time-injection pattern: `record_failure(now: Instant)` for deterministic testing
- Backoff computed via loop (not `2^n`) to avoid overflow

### Error Hierarchy
```
FrameworkError
├── Worker(WorkerError)
│   ├── Unreachable { pool, reason }
│   ├── Timeout { pool, elapsed_ms }
│   ├── ReadinessTimeout { pool, timeout_ms }
│   ├── ProtocolViolation { pool, message }
│   ├── WorkerReturnedError { pool, message }
│   └── PermanentFailure { pool, failures, window_ms }
├── Client(ClientError)
│   ├── InvalidBody(String)
│   ├── PayloadTooLarge { limit, actual }
│   └── UnsupportedContentType(String)
├── Gateway(GatewayError)
│   ├── ResourceExhaustion
│   ├── ShutdownInProgress
│   └── Internal(String)
└── Config(ConfigError)
```

### HTTP Error Mapping
- Worker errors → 502/503/504 (upstream errors)
- Client errors → 400/413/415 (client errors)
- Gateway errors → 503/500 (server errors)
- 5xx logged as `error!`, 4xx logged as `warn!`

## Rust Conventions

### General
- **Edition:** 2021
- **MSRV:** 1.85
- **License:** MIT OR Apache-2.0
- **Workspace dependencies:** All deps declared at workspace level, inherited via `{ workspace = true }`
- Run `cargo fmt` before committing
- Run `cargo clippy` to catch idiom issues
- Warnings should be fixed before commit, not accumulated

### Style
- snake_case for variables, fields, functions
- PascalCase for types
- SCREAMING_SNAKE for constants
- No unnecessary parentheses around `if` conditions
- Prefer `is_none()` over `!is_some()`
- Use `while let Some(&x) = collection.front()` pattern instead of `is_empty() + unwrap()`
- Use `as usize` for bounded conversions instead of `try_from().expect()`

### Error Handling
- Library crates use typed errors via `thiserror`
- Binary crate (`sky-gateway`) uses `anyhow` at the top level
- Use `?` operator for propagation; use `match` when cleanup is needed on the error path
- `HttpError` newtype wraps `FrameworkError` for orphan-rule compliance with axum's `IntoResponse`

### Async Patterns
- `tokio::spawn(async move { ... })` for background tasks; clone `Arc`s before the `async move` block
- `tokio::select!` for racing futures (child.wait vs. shutdown_token.cancelled)
- `tokio::time::sleep` (never `std::thread::sleep`) for delays in async code
- `tokio::time::timeout` for bounding waits
- Don't use `std::sync::Mutex` in async code; use `tokio::sync::Mutex` if needed (but prefer `ArcSwap` or channels)

### Key Crate Choices
- `axum` 0.7 — HTTP server
- `tokio` (full features) — Async runtime
- `tokio-util` 0.7 — `CancellationToken`
- `arc-swap` — Lock-free atomic pointer swaps for `WorkerSocket` replacement on restart
- `bytes` — Zero-copy byte buffer for RESPONSE_CHUNK frames
- `rmp-serde` — MessagePack serialization for INVOKE / RESPONSE_HEAD / ERROR frames
- `serde_bytes` — Correct binary field encoding in MessagePack (required for body field)
- `tower` 0.5 / `tower-http` 0.6 — Middleware layers
- `tracing` / `tracing-subscriber` — Structured logging
- `serde` / `serde_json` / `toml` — Serialization
- `humantime-serde` — Human-readable durations in config ("10s", "500ms")
- `thiserror` — Typed error derive macros
- `anyhow` — Binary-level error convenience
- `clap` 4 (derive) — CLI argument parsing
- `jsonschema` — JSON Schema validation for request bodies

### Testing
- Unit tests inside `#[cfg(test)] mod tests` within source files
- Integration tests in `crates/*/tests/` directory (sibling to `src/`)
- Integration tests use `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
- Unique worker IDs per test via `"test-{name}-{pid}-{nanos}"` format to avoid socket collisions
- `env!("CARGO_MANIFEST_DIR")` to locate workspace-relative paths
- Time-injection pattern for deterministic policy testing
- Doctests run external to crate: must use crate-name imports, not `crate::`

### Config Pattern
- Serde-friendly structs with `#[serde(default = "fn_name")]` for optional fields
- `humantime_serde` for Duration fields
- `validate()` method separate from deserialization
- `WorkerConfig::new(bun_path, worker_script, worker_version)` — 3 required fields
- `resolve_binary()` helper does PATH search for bare names (matching `Command::new` semantics)
- Unknown fields in TOML are silently ignored (no `deny_unknown_fields`) — forward-compatible

## TypeScript / Worker Conventions

### General
- **Runtime:** Bun
- **Strict TypeScript:** `strict: true`, `noUncheckedIndexedAccess`, `verbatimModuleSyntax`
- **Module resolution:** `bundler`
- **Path aliases** (in `tsconfig.json`):
  - `@gen/*` → `./gen/*`
  - `@sky/decorators` → `./src/decorators`
  - `@sky/emitter` / `@sky/emitter/*` → `./src/emitter/*`
  - `@sky/runtime/*` → `./src/runtime/*`
  - `@sky/transport` → `./src/transport`
- `"type": "module"` in package.json

### `startServer` API

The application entrypoint passes all `@Service`-decorated classes to `startServer`. It handles DI container setup, service registry, dispatcher wiring, and socket connection internally:

```typescript
import { startServer, logger } from "sky/runtime";
import { UserService } from "./services/user";
import { HealthService } from "./services/health";

const server = await startServer({
  workerVersion: "1.0.0",
  logger,
  gracePeriodDefaultMs: 5000,
  services: [UserService, HealthService],
});
```

Do NOT manually create `Container`, `ServiceRegistry`, or `HandlerDispatcher` in application entrypoints.

### Decorator API

```typescript
import { Service, Handler, Body, Param, Query, Header, ZodBody, Group, Context } from "sky/decorators";
import { HttpError } from "sky/runtime";
import type { RequestContext } from "sky/runtime";
import { z } from "zod";

@Service({ lifetime: "singleton" | "scoped" })
@Group({ prefix: "/api/admin" })          // optional — prefixes all handlers in the class
class MyService {
  @Handler({ method: "GET", path: "/items/:id", extract: { id: Param("id") } })
  async getItem({ id }: { id: string }) { ... }

  @Handler({ method: "POST", path: "/items", extract: { body: ZodBody(CreateSchema) } })
  async createItem({ body }: { body: z.infer<typeof CreateSchema> }) {
    return new Response(result).status(201);
  }

  @Handler({ method: "GET", path: "/search", extract: { q: Query("q"), userId: Header("x-user-id") } })
  async search({ q, userId }: { q?: string; userId?: string }) { ... }

  // Context<TBody> injects the full request context including JWT claims and typed body
  @Handler({ method: "POST", path: "/secure", extract: { ctx: Context<CreateSchema>() } })
  async secureEndpoint({ ctx }: { ctx: RequestContext<CreateSchema> }) {
    const sub = ctx.claims?.sub;  // typed JWT claims
    const body = ctx.body;        // typed as CreateSchema
    const requestId = ctx.requestId;
  }
}
```

The `extract` record maps field names to descriptor factories. Available extractors:
- `Body<T>()` — raw body (parsed JSON, typed via generic)
- `ZodBody(schema)` — Zod-validated and typed body (also emits JSON Schema for gateway validation)
- `Param("name")` — URL path parameter
- `Query("name")` — query string parameter
- `Header("name")` — request header (lowercased)
- `Context<TBody>()` — full `RequestContext<TBody>`: requestId, method, path, params, query, headers, claims (parsed JWT), body (typed via generic). Runtime-only; never emitted to the manifest.

Throwing `HttpError(status, message)` from a handler sends an error frame back to the gateway, which maps it to the appropriate HTTP response.

### Response Wrapper

Handlers can return a plain object (status comes from manifest default) or a `Response` for explicit control:

```typescript
import { Response } from "sky/runtime";

// Explicit status
return new Response(body).status(201);

// Headers and cookies
return new Response(body)
  .header("x-trace-id", traceId)
  .cookie("session", token, { httpOnly: true, secure: true });

// Clear a cookie
return new Response(null).status(204).clearCookie("session");
```

### Manifest Emitter

The emitter (`sky/emitter`) is a build-time tool that walks decorated classes using TypeScript reflection (`Reflect.getMetadata`) and emits `sky-manifest.json`. The manifest is the contract between the TS decorator layer and the Rust gateway.

Manifest structure:
```json
{
  "version": "1",
  "hash": "sha256:...",
  "emitted_at": "ISO-8601",
  "services": [{
    "name": "MyService",
    "className": "MyService",
    "lifetime": "scoped",
    "handlers": [{
      "name": "createItem",
      "method": "POST",
      "path": "/items",
      "status": 201,
      "validate": true,
      "extract": [{ "source": "body", "position": 0, "schema": { ... } }],
      "response": { ... }
    }]
  }],
  "middleware": [],
  "schemas": {}
}
```

### Worker Transport (Sky Framing — TS side)

`SkyWorkerSocket` in `src/transport/socket.ts` connects to the gateway-owned UDS and implements the Sky framing protocol from the worker's perspective:
- Reads INVOKE frames and yields them via `invocations()` async generator
- Sends RESPONSE_HEAD, RESPONSE_CHUNK, RESPONSE_END via `sendHead`, `sendChunk`, `sendEnd`
- Sends ERROR via `sendError`
- Responds to PING with PONG automatically
- Calls `onDrained()` hook when DRAIN is received (triggers graceful shutdown)
- Calls `onSocketClosed()` hook if the connection drops unexpectedly

The TS worker does **not** bind or create the socket — it connects to the path derived from `SKY_WORKER_ID`.

### `exactOptionalPropertyTypes` Pattern
When an interface has `foo?: T`, you cannot pass `foo: undefined`. Conditionally build the object instead:
```typescript
const options: LoggerOptions = { level: "info" };
if (isDev) {
    options.transport = { target: "pino-pretty", ... };
}
```

### Dependencies
- `@blue.ts/di` — DI container (scoped and singleton lifetimes)
- `@msgpack/msgpack` — MessagePack encode/decode in the transport layer
- `pino` — Structured logging
- `pino-pretty` — Dev-only log formatting
- `zod` — Schema validation for `@ZodBody`

## Phase Progress

### E1 — Core Pipeline (Complete)
- [x] E1-S1: Cargo workspace skeleton
- [x] E1-S2: sky-runtime error types and primitives
- [x] E1-S3: Bun worker bootstrapped with DI
- [x] E1-S4: Worker supervisor with integration tests
- [x] E1-S5: Gateway binary with config-driven startup
- [x] E1-S6: Restart policy with exponential backoff and crash-loop detection
- [x] E1-S7: Graceful shutdown sequence (DRAIN frame, in-flight draining, ordered cleanup)

### E2 — Manifest & Routing (Complete)
- [x] E2-S1: Decorator system (`@Service`, `@Handler`, `@Body`, `@Param`, `@Query`, `@Header`, `@Group`, `@ZodBody`, `@Context`)
- [x] E2-S2: Type walker and JSON Schema emitter (reflection over decorated metadata)
- [x] E2-S3: Schema registry with `$ref` deduplication
- [x] E2-S4: Manifest assembler — produces `sky-manifest.json`
- [x] E2-S5: Replace gRPC/Connect boundary with Sky framing protocol (custom binary over UDS)
- [x] E2-S6: Manifest-driven router in the gateway (axum routes from manifest, JSON Schema validation)
- [x] E2-S7: Dispatcher in the worker (manifest-driven handler resolution, DI per request)

### E3-A — Worker Pool Manager (Complete)
**The performance unlock.** Delivered 340–360k req/s clean on a 10-worker pool (AMD Ryzen AI MAX+ 395).

- [x] `WorkerPool` in `crates/sky-worker/src/pool.rs` — owns `Vec<Supervisor>`, round-robin via `AtomicUsize`
- [x] `RouterState` holds `Option<Arc<WorkerPool>>`; `generic_handler` calls `pool.acquire()`
- [x] Pool size driven by `[worker].pool_size` in `sky.toml`; bench config uses 10 workers
- [x] Per-supervisor independent crash detection, restart policy, and `ArcSwap<WorkerSocket>` hot-swap
- [x] `pool.shutdown()` sends DRAIN to all workers concurrently via `futures::future::join_all`
- [x] Per-worker transport diagnostics (write-lock wait, inflight count, frames routed/dropped logged every 1s)

### E3-B — Middleware Pipeline (Complete)
**The framework completeness unlock.**

- [x] Middleware execution on the **gateway side** (Rust): `cors.rs`, `rate_limit.rs`, `auth.rs` run before forwarding INVOKE. Each is per-route config compiled from the manifest at startup into `RouterState`.
- [x] Middleware execution on the **worker side** (TS): dispatcher filters `kind === "user"` entries, resolves each class from `middlewareMap` (populated by `startServer({ middleware: [...] })`), and runs them via `runMiddlewareChain` around the handler leaf.
- [x] HTTP chunked streaming to clients: `build_streaming_response` in `router.rs` uses `Body::from_stream(ReceiverStream)` — selected per-route via the `streaming` flag in the manifest.
- [x] Built-in middleware: `cors`, `rateLimit`, `requireAuth` exported from `sky/runtime`; `issueToken` / `verifyToken` for the worker side of JWT.
- [x] Decorator API: `@Handler({ middleware: [requireAuth(), cors(...)] })` per-handler; `@Middleware` on a service class applies to all its handlers via service-level manifest merge in the dispatcher.
- [ ] `requestLogger` built-in middleware — deferred; request logging is handled by gateway-level `tracing` spans rather than a user-facing middleware.

### E3-C — Request Context + DI (Complete)
**The developer experience unlock.**

- [x] `RequestContext<TBody>` interface — carries `requestId`, `handlerId`, `method`, `path`, `params`, `query`, `headers`, `claims` (parsed from `x-sky-claims`), and `body` (typed via the `TBody` generic).
- [x] `Context<TBody>()` extractor — new descriptor alongside `Param()`, `Header()`, etc. Declared in `extract: { ctx: Context<CreateDto>() }`. The dispatcher populates it from the live `SkyInvocation` at the same point all other extractors are resolved.
- [x] Claims are automatically parsed from the `x-sky-claims` header injected by the gateway auth middleware — no manual `JSON.parse` in handlers.
- [x] Emitter skips context descriptors (`source === "context"`) so they never appear in `sky-manifest.json` — the gateway has no concept of them.
- [x] Exported as `Context` from `sky/decorators` and `RequestContext` type from `sky/runtime`.
- [x] 12 unit + assembler tests covering field population, claims parsing, invalid-JSON fallback, empty body, and manifest skip.

### E3-D — CLI Tooling (Complete)
- [x] `sky build` CLI: scans `@Service`-decorated files, runs the type walker and manifest assembler, writes `sky-manifest.json`
- [x] `sky build --watch` / `-w`: debounced file watcher; re-resolves entry points from `sky.toml` on each change (uses configured `sources`, not a hardcoded default)
- [x] Config loading from `sky.toml` via `[build].sources`; defaults to `./src/services` if omitted
- [x] Removed all proto/buf steps — CLI is now purely a manifest emitter

## `sky` CLI

The `sky` CLI (`worker/src/cli.ts`) is the build tool for Sky projects. It reads `sky.toml`, imports service modules to trigger decorator registration, runs the type walker and manifest assembler, and writes `sky-manifest.json`.

```bash
sky build              # One-shot build — exits 0 on success, 1 on error
sky build --watch      # Watch mode — rebuilds on any .ts change in source dirs
sky build -c path/to/sky.toml   # Use a non-default config location
```

### How it works

1. **Config** — Loads `sky.toml` from `cwd` (or `--config`). Reads `[build].sources` (default: `["./src/services"]`) and `[worker].manifest_path` (default: `./sky-manifest.json`).
2. **Entry point resolution** — Globs `**/*.ts` under each source directory, skipping `.test.ts`, `.spec.ts`, and `.d.ts` files.
3. **Manifest assembly** — Imports each entry file, which triggers `@Service`/`@Handler` decorator registration. The type walker reflects over registered metadata and the assembler produces the manifest JSON.
4. **Output** — Writes `sky-manifest.json` to `manifest_path`. Logs service count, handler count, schema count, and elapsed time.

### Watch mode

Watch mode wraps the build in a `fs.watch` loop over each discovered source directory. A 200ms debounce prevents redundant rebuilds on rapid saves. On each rebuild it re-resolves entry points from `sky.toml` so newly added or deleted files are picked up automatically.

### `sky.toml` `[build]` section

```toml
[build]
# Directories to scan for @Service/@Handler decorated classes.
# Paths are relative to the directory containing sky.toml.
sources = ["./src/services"]
```

`[worker].manifest_path` controls where the manifest is written (default: `./sky-manifest.json`, relative to `sky.toml`).

## Development Workflow

### Rust Side
```bash
cd sky
cargo build                    # Build everything
cargo test -p sky-worker       # Test specific crate
cargo test -p sky-worker --test supervisor_integration  # Integration tests only
cargo clippy                   # Lint
cargo fmt                      # Format
```

### TypeScript Side
```bash
cd sky/worker
bun install                    # Install dependencies
bun run typecheck              # Type-check without emitting
bun run src/cli.ts build       # Build sky-manifest.json (one-shot)
bun run src/cli.ts build -w    # Build and watch for changes
SKY_WORKER_ID=1 bun run src/index.ts   # Run worker (gateway must already be running)
```

### Running the Gateway
```bash
cd sky
cargo run -p sky-gateway                    # Uses ./sky.toml
cargo run -p sky-gateway -- --config path   # Custom config
```

### Testing Endpoints
```bash
# Hello (smoke test)
curl -X POST http://127.0.0.1:8080/hello \
  -H "Content-Type: application/json" \
  -d '{"name": "World"}'

# Test restart: kill the worker, gateway should auto-restart it
kill -9 $(pgrep -f "bun run src/index.ts")
# Retry the curl — should succeed after brief backoff
```

## Key Technical Learnings

1. **Sky framing replaces gRPC.** A 10-byte binary header + MessagePack payloads is simpler, faster, and removes the need for proto codegen. The gateway and worker share the same frame format documented above.
2. **Gateway owns the socket; worker connects.** The reverse (worker binds) requires the gateway to know the socket exists before connecting, introducing a race. Having the gateway bind first eliminates that.
3. **`serde_bytes` is required for binary fields in MessagePack.** Without it, `&[u8]` serializes as an array of integers rather than a binary blob. Use `#[serde(with = "serde_bytes")]` on any `&[u8]` or `Vec<u8>` field in a MessagePack payload.
4. **Do not override `current_dir` when spawning the worker.** The worker inherits the gateway's working directory so that `process.cwd()/sky-manifest.json` resolves to the manifest alongside `sky.toml`. Bun resolves tsconfig path aliases relative to the entry file, not cwd, so `@sky/*` imports are unaffected.
5. **The TS worker must never delete the socket file.** It is owned by the gateway. Deleting it on startup causes the gateway's listener to break. The gateway's `SkyListener::bind()` handles stale socket cleanup.
6. **`Command::new("bun")` does PATH resolution; `Path::exists("bun")` does not.** Config validation must match `Command::new` semantics via a custom `resolve_binary()` helper.
7. **Rust's orphan rule** prevents implementing external traits for external types. Use newtypes (`HttpError` wrapping `FrameworkError`).
8. **Types implementing `Drop` cannot have fields moved out.** If you need to consume fields in an async cleanup path, don't implement `Drop`.
9. **`ArcSwap` is lock-free atomic pointer swap.** Reads are cheap (no lock). Use `Arc<ArcSwap<WorkerSocket>>` so the monitor task can swap in a new connection on restart without blocking in-flight RPCs.
10. **Clone `Arc`s before `async move` blocks.** The closure captures by value; the original stays for other uses.
11. **`CancellationToken` is the coordination mechanism** between commanded shutdown and the monitor task. Cancel first, then kill — ordering prevents restart races.
12. **Doctests run external to crate.** Use crate-name imports (`sky_runtime::X`), not `crate::X`.
13. **Top-level `await` in TS modules runs at import time.** `dispatcher.ts` uses `await import(manifestPath)` at the top level — it runs when the module is first imported, before any handler is called. A missing manifest causes an immediate crash.

## Benchmarks

All results: commit `64a5e0a`, AMD Ryzen AI MAX+ 395 (16-core), Linux 6.19.13, 10-worker pool (`pool_size = 10`).

### Standard (2k req/s, wrk2, 4 threads, 100 connections, 30s)

| Endpoint | p50 | p99 | p99.9 |
|---|---|---|---|
| POST /hello (unary) | 0.76ms | 1.84ms | 2.47ms |
| GET /stream/events?count=10 | 0.79ms | 1.92ms | 4.16ms |
| GET /stream/fibonacci?limit=20 | 0.82ms | 2.05ms | 3.29ms |

Zero errors across all three. Streaming overhead vs unary: ~70µs p50.

### Saturation Ramp (POST /hello)

Best clean run (2026-05-04 02:40 UTC):

| Target req/s | Actual req/s | p50 | p99 |
|---|---|---|---|
| 340k | 338,625 | 0.84ms | 3.01ms |
| 360k | 352,876 | 1.09ms | 5.84ms |
| 380k | 377,810 | 29ms | 101ms ← saturation |

**Effective ceiling: ~360k req/s** (p99 < 6ms). Saturation at ~375–380k. Ramp results vary run-to-run due to OS scheduler variance; 340–360k is consistently clean.

### TTFB (concurrency=10, 100 runs)

| Endpoint | p50 | p99 |
|---|---|---|
| POST /hello | 0.27ms | 0.67ms |
| GET /stream/events?count=10 | 0.38ms | 2.31ms |
| GET /stream/fibonacci?limit=20 | 0.36ms | 0.77ms |

### Isolation

/hello with concurrent fibonacci streaming: **p50=0.79ms, p99=1.99ms** vs baseline p50=0.80ms, p99=2.10ms. Worker pool isolation working correctly.

## Future Architecture Notes

### Rolling Reload (Phase 6/7)
Zero-downtime code reload via SIGHUP signal. Spawn new worker, health-check, swap into pool, drain old worker's in-flight requests, shut down old worker. Already architecturally supported by `spawn_and_ready()`, `ArcSwap<WorkerSocket>`, `CancellationToken`.

### External Services (Phase 7)
Inter-service communication via `[[services]]` config section. Depends on HostRuntime service (Phase 4), resilience layer (Phase 7). Versioned config (`version = "1"`) accommodates future schema additions.

### Polyglot Workers (Far Future)
The worker contract is the Sky framing protocol, not a TypeScript convention. Any language can implement a Sky worker by:
1. Connecting to `/tmp/sky/workers/sky-worker-{SKY_WORKER_ID}.sock`
2. Parsing the 10-byte frame header
3. Decoding INVOKE (MessagePack), responding with RESPONSE_HEAD + chunks + END (or ERROR)
4. Responding to PING with PONG
5. Handling DRAIN gracefully

TypeScript is first-class; others are additive.

## Related Projects

- **Blue.TS** — John's Bun-native TypeScript web framework with per-request DI. Sky reuses `@blue.ts/di` as the DI substrate. Local path: `~/Desktop/blue.ts`
- **Cowork** — Desktop tool where project documentation and phased roadmaps are stored.

## Delivered Documents

- `ts-rust-framework-outline-v3.docx` — Architecture outline (Draft 3)
- `phase1-spec.docx` — Phase 1 technical specification
- `agile-plan.docx` — 9 Epics × 60+ Stories with metadata
- `architecture-flow.mermaid` — Worker pools, outbox loop, HostRuntime flow diagram
