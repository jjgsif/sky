# Claude.md — Sky Framework Transport Protocol

## Context

This document captures architectural decisions, design rationale, and implementation details established in the conversation covering the Sky custom transport protocol. It is intended as a reference for future conversations so context does not need to be re-established.

---

## Project Overview

Sky is a manifest-driven TypeScript/Rust framework. The Rust gateway handles routing, validation, and middleware. TypeScript workers handle business logic via decorated service classes. The manifest (`sky-manifest.json`) produced by `sky build` is the shared contract between the two.

The epics in scope:
- **E2** — Manifest emitter, decorator API, schema-driven gateway
- **E3** — Worker pools, middleware, admin API

---

## Transport Protocol (E2-S12)

### Decision: Replace gRPC with a custom binary framing protocol

gRPC was the original transport between the Rust gateway and TypeScript workers. It was replaced with a custom protocol for the following reasons:

- Sky's build pipeline already owns type safety via the manifest and JSON Schema validation in Rust. gRPC's typed contracts were redundant.
- gRPC over HTTP/2 carries significant overhead (HPACK, Protobuf encoding, HTTP/2 framing) for a same-machine Unix socket hop where none of that complexity is justified.
- The custom protocol is 3–5× faster for unary responses and significantly faster for streaming workloads.
- Runtime isolation is preserved — Rust and TypeScript share no memory. The Unix socket is the only boundary.

### Architecture

```
HTTP Client
  ↓ HTTP/1.1 or HTTP/2
Rust Gateway
  ├─ Route matching          (manifest-driven, unchanged)
  ├─ JSON Schema validation  (pre-dispatch, unchanged)
  ├─ Frame serialization     ← custom protocol
     ↓ Unix domain socket
Sky Frame Protocol
     ↓
Worker Process (TypeScript)
  ├─ Frame deserialization
  ├─ Handler dispatch + DI
  └─ Frame serialization (response)
     ↑ Unix domain socket
Rust Gateway
  └─ Stream frames → HTTP response
```

**Rust owns the socket.** The gateway creates and binds the Unix domain socket file. The worker connects to it as a client. This is critical for the restart model — when a worker crashes and restarts, it reconnects to the same socket the gateway is still holding.

### Frame Format

Fixed 10-byte header on every frame in both directions:

```
[0]     version     (u8)  — always 0x01
[1]     frame_type  (u8)  — see frame types below
[2..6]  request_id  (u32, big-endian)
[6..10] payload_len (u32, big-endian)
```

### Frame Types

| Byte | Name             | Direction         | Description                                      |
|------|------------------|-------------------|--------------------------------------------------|
| 0x01 | INVOKE           | Gateway → Worker  | Handler invocation. One per request.             |
| 0x02 | RESPONSE_HEAD    | Worker → Gateway  | Status code and headers. Must be first response. |
| 0x03 | RESPONSE_CHUNK   | Worker → Gateway  | Body chunk. Zero or more per request.            |
| 0x04 | RESPONSE_END     | Worker → Gateway  | End of response body. No payload.                |
| 0x05 | ERROR            | Worker → Gateway  | Unrecoverable handler error.                     |
| 0x06 | PING             | Either direction  | Keepalive.                                       |
| 0x07 | PONG             | Either direction  | Keepalive reply.                                 |
| 0x08 | DRAIN            | Gateway → Worker  | Graceful shutdown signal.                        |

### Payload Encoding

- Structured payloads (INVOKE, RESPONSE_HEAD, ERROR) — **MessagePack** via `rmp-serde` (Rust) and `@msgpack/msgpack` (TypeScript).
- Raw body chunks (RESPONSE_CHUNK) — **unencoded bytes**. Zero copies, zero transformation.

MessagePack was chosen over Protobuf because the manifest already owns the schema. The transport layer does not need to re-encode it. JSON was ruled out for performance reasons.

### Multiplexing

A single Unix socket is shared across all concurrent requests. The `request_id` field in every frame header is the multiplexing primitive. The gateway assigns it on INVOKE; the worker echoes it on every response frame. The gateway's `in_flight: HashMap<u32, ResponseTx>` routes each inbound frame to the correct HTTP response channel by `request_id`.

Frame interleaving between requests is expected and handled correctly by `request_id` routing. Byte interleaving within a frame is prevented by writing each frame as a single `Buffer.concat([header, payload])` call. Under backpressure (large payloads, kernel buffer full) a write queue serialises frames to prevent partial writes from interleaving.

### Streaming

Every response is a stream. A unary JSON response is a degenerate stream — one RESPONSE_HEAD, one RESPONSE_CHUNK, one RESPONSE_END. This eliminates branching in the gateway dispatch path. The HTTP client starts receiving data when the first RESPONSE_CHUNK arrives, not when the handler finishes.

**Response lifecycle:**
```
INVOKE → RESPONSE_HEAD → RESPONSE_CHUNK(s) → RESPONSE_END
```

**RESPONSE_HEAD is immutable once sent.** The HTTP response head is flushed to the client immediately. An ERROR frame arriving after RESPONSE_HEAD is a protocol violation — the connection is closed.

### Request Body Streaming

The body in INVOKE is currently a fully buffered `Uint8Array` — validated by Rust JSON Schema before dispatch. Streaming request bodies are a future extension requiring new frame types (REQUEST_CHUNK, REQUEST_END) and a `StreamedBody()` extract descriptor.

---

## Rust Crate Changes (E2-S12)

**Removed:**
- `tonic` — gRPC stack
- `prost` — Protobuf encoding
- `proto/` directory — HandlerInvocation, HandlerResponse, GreetRequest

**Added:**
- `rmp-serde` — MessagePack serialization
- `async-stream` — streaming body helper

**New types:**
- `FrameHeader` — encodes/decodes the 10-byte header
- `FrameType` — discriminant enum for all frame types
- `WorkerConnection` — owns the Unix socket, in-flight map, background reader task
- `InboundFrame` — enum of decoded inbound frame variants
- `dispatch_to_worker()` — replaces gRPC dispatch in E2-S8

---

## TypeScript Package Changes

### @sky/transport (new internal package)

`SkyWorkerSocket` — client-side framing protocol implementation.

Key design points:
- Static factory `SkyWorkerSocket.connect(socketPath)` with retry loop — waits for the gateway to create the socket before connecting. Timeout matches `spawn_timeout` in `sky.toml`.
- `readBuf` accumulation handles TCP not respecting frame boundaries.
- `InvocationResult` discriminated union — `{ ok: true, inv }` or `{ ok: false, reason: "drain" | "socket_closed" }`. Prevents type lies from passing `null` through a typed callback.
- `resolveWaiters(reason)` — single method resolving all parked Promise callbacks on both shutdown paths.
- `onDrained(cb)` / `onSocketClosed(cb)` — lifecycle hooks for `server.ts`.
- Write queue serialises outbound frames under backpressure.

**Shutdown paths:**
- **DRAIN frame** — gateway-initiated graceful shutdown. Generator stops yielding; in-flight handlers run to completion.
- **Socket close** — unexpected closure. All parked waiters resolved immediately; process exits.

### @sky/worker (updated)

**`server.ts`**
- Removed: `connectNodeAdapter`, `http2`, `ConnectRouter`, `WorkerControl`, `workerState`
- Added: `Container`, `ServiceRegistry`, `SkyWorkerSocket`, `createDispatcher`
- `services` array replaces `dispatcher.registerServices(router)` — caller passes service classes explicitly
- Composition root: creates Container → ServiceRegistry → dispatcher → socket

**`dispatcher.ts`**
- Removed: gRPC stub calls, `getHandlerMetadata` from decorators, `__sky_registry.ts`
- `HandlerDispatcher` interface with `start(socket): () => void`
- `createDispatcher(registry)` — receives `ServiceRegistry`, not a socket path
- Extract descriptors sourced from `ServiceRegistry` (populated by `@sky/decorators` at decoration time)
- Status code and `validate` flag sourced from `sky-manifest.json` (gateway concerns, not decorator concerns)
- `normalize()` — wraps bare handler return values into full response envelope using manifest-defined default status

---

## Decorator API

Stage 3 TC39 decorators — **not** `reflect-metadata` / `emitDecoratorMetadata`.

Parameter binding is explicit via the `extract` array on `@Handler`:

```typescript
@Handler({
  method:  "POST",
  path:    "/users",
  extract: [Body()],
})
async createUser(body: { name: string }) { ... }
```

**Extract descriptor factories:**
- `Body()` — fully buffered, deserialized, pre-validated object
- `StreamedBody()` — `AsyncGenerator<Uint8Array>`, no buffering, no JSON Schema validation
- `Query(name)` — URL query parameter
- `Param(name)` — path parameter
- `Header(name)` — request header

`StreamedBody()` vs `Body()` is a build-time signal — the emitter emits no JSON Schema for `StreamedBody` handlers and the gateway skips validation for those routes.

---

## Service Registry

`ServiceRegistry` reads from `ServiceMap` populated by `@sky/decorators` at decoration time. No generated registry file. The `ServiceMap` is the source of truth on the worker side for:
- Service class constructors
- Lifetimes (`singleton`, `scoped`, `transient`)
- Dependencies (for DI factory construction)
- Handler extract descriptors

`sky-manifest.json` is the source of truth for:
- Route paths and HTTP methods
- JSON Schema validation (gateway-side)
- Default response status codes
- Middleware assignments

---

## Manifest Format (relevant subset)

```json
{
  "services": [{
    "name": "HelloService",
    "className": "HelloService",
    "lifetime": "singleton",
    "dependencies": [],
    "handlers": [{
      "name": "greet",
      "method": "POST",
      "path": "/hello",
      "status": 201,
      "validate": true,
      "extract": [{
        "source": "body",
        "position": 0,
        "schema": {
          "type": "object",
          "properties": { "name": { "type": "string" } },
          "required": ["name"]
        }
      }],
      "response": {
        "type": "object",
        "properties": { "message": { "type": "string" } },
        "required": ["message"]
      }
    }]
  }]
}
```

---

## DI Container (@blue.ts/di)

```typescript
// Registration
container.register(ServiceClass, {
  lifetime: "singleton" | "scoped" | "transient",
  factory:  autowire(ServiceClass, [DepToken, OtherToken]),
});

// Scoped resolution — one scope per request
const scope   = container.createScope();
const service = await scope.get(ServiceClass);
```

`"scoped"` lifetime maps to what the E2 plan called `"request"` scope — a fresh instance per `createScope()` call. Singletons resolve from the root container through any scope automatically.

---

## HTTP Coverage

The `(status, headers, stream of bytes)` model covers all HTTP/1.1-derived protocols natively:
- JSON APIs — unary, single chunk
- HTML — unary or progressive (chunked)
- SSE — long-lived generator, `text/event-stream`
- File downloads — generator over file read stream
- NDJSON, multipart, CSV — any content type

**Not covered:**
- WebSockets — require bidirectional protocol upgrade, handled at gateway level
- HTTP/2 Server Push — gateway-level concern, largely deprecated
- Streaming request bodies — future extension (REQUEST_CHUNK frames)
- HTTP trailing headers — obscure, low priority

---

## Background Jobs

Fire-and-forget dispatch via a future `INVOKE_BACKGROUND` frame (0x09). Worker sends this to the gateway to enqueue work on another handler. Gateway dispatches to an available worker; no result is returned to the caller. Optional `schedule_at` timestamp for deferred execution.

This model deliberately avoids synchronous inter-worker calls. If two handlers need to compose results synchronously they belong in the same service. The gateway is a dispatcher, not an orchestrator.

---

## Known Issues / Open Items

- **Socket startup race** — worker must not attempt connection before Rust gateway has created the socket. Fix: `SkyWorkerSocket.connect()` static factory with poll-retry loop. Root cause: Rust supervisor must create and bind socket before spawning worker process.
- **Streaming request bodies** — currently buffered in full before dispatch. Large file uploads need REQUEST_CHUNK / REQUEST_END frames and `StreamedBody()` extract descriptor.
- **Write queue** — `writeRaw` must serialise outbound frames through a queue to handle backpressure correctly for large payloads. Single `Buffer.concat` write is safe for typical payloads but not under kernel buffer pressure.
