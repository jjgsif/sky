# sky-framework

The TypeScript/Bun SDK for [Sky](https://github.com/sky-framework/sky) — decorators, runtime, manifest emitter, and the `sky` CLI. This package is what Sky worker processes import to define and serve handlers.

```bash
bun add sky-framework
```

## Package exports

| Export | Contents |
|---|---|
| `sky-framework/decorators` | `@Service`, `@Handler`, `@Group`, `@Middleware`, `Body`, `ZodBody`, `Param`, `Query`, `Header`, `Context` |
| `sky-framework/runtime` | `startServer`, `logger`, `HttpError`, `Response`, `requireAuth`, `cors`, `rateLimit`, `issueToken`, `verifyToken`, `RequestContext` |
| `sky-framework/emitter` | Programmatic manifest assembly (advanced / build-tool use) |

---

## `startServer`

The worker entry point calls `startServer` once. It wires DI, the dispatcher, and the Sky framing socket internally.

```ts
import { startServer, logger } from "sky-framework/runtime";
import { UserService } from "./src/services/user";
import { AuthService } from "./src/services/auth";

const server = await startServer({
  workerVersion: "0.1.0",   // must match [worker].worker_version in sky.toml
  logger,
  services: [UserService, AuthService],
  middleware: [],            // optional — worker-side middleware classes
  gracePeriodDefaultMs: 5000,
});

logger.info({ workerId: server.workerId }, "worker ready");

process.on("SIGINT", () => server.close());
process.on("SIGTERM", () => server.close());
```

Do not manually construct `Container`, `ServiceRegistry`, or `HandlerDispatcher` — `startServer` owns that wiring.

---

## Decorator API

```ts
import { Service, Handler, Group, Body, Param, Query, Header, ZodBody, Context } from "sky-framework/decorators";
import type { RequestContext } from "sky-framework/runtime";
import { z } from "zod";

@Service({ lifetime: "singleton" | "scoped" })
@Group({ prefix: "/api" })       // optional — prefixes all handlers in the class
class MyService {

  // Path parameter
  @Handler({ method: "GET", path: "/items/:id", extract: { id: Param("id") } })
  async getItem({ id }: { id: string }) { ... }

  // Zod-validated body (also validated at the gateway before the INVOKE is forwarded)
  @Handler({ method: "POST", path: "/items", extract: { body: ZodBody(CreateSchema) } })
  async createItem({ body }: { body: z.infer<typeof CreateSchema> }) {
    return new Response(result).status(201);
  }

  // Query string and header extractors
  @Handler({ method: "GET", path: "/search", extract: { q: Query("q"), token: Header("x-token") } })
  async search({ q, token }: { q?: string; token?: string }) { ... }

  // Raw body (JSON-parsed, typed via generic)
  @Handler({ method: "PUT", path: "/items/:id", extract: { id: Param("id"), body: Body<UpdateDto>() } })
  async updateItem({ id, body }: { id: string; body: UpdateDto }) { ... }

  // Full request context: requestId, method, path, params, query, headers, claims, body
  @Handler({ method: "POST", path: "/secure", extract: { ctx: Context<CreateSchema>() } })
  async secureEndpoint({ ctx }: { ctx: RequestContext<CreateSchema> }) {
    const sub = ctx.claims?.sub;  // JWT claims injected by the gateway auth middleware
    return { user: sub, body: ctx.body };
  }
}
```

### Extractors

| Extractor | Type | Description |
|---|---|---|
| `Body<T>()` | `T` | Raw request body, JSON-parsed and typed |
| `ZodBody(schema)` | `z.infer<typeof schema>` | Zod-validated body; schema also emitted to manifest for gateway validation |
| `Param("name")` | `string` | URL path parameter |
| `Query("name")` | `string \| undefined` | Query string parameter |
| `Header("name")` | `string \| undefined` | Request header (lowercased) |
| `Context<TBody>()` | `RequestContext<TBody>` | Full request context with typed body and JWT claims |

`Context<TBody>()` is runtime-only — it is never emitted to `sky-manifest.json`.

---

## Response wrapper

Return a plain object to use the default status from the manifest (GET → 200, POST → 201, etc.), or wrap with `Response` for explicit control:

```ts
import { Response } from "sky-framework/runtime";

// Explicit status
return new Response(body).status(201);

// Custom headers
return new Response(body).header("x-trace-id", traceId);

// Set a cookie
return new Response(body).cookie("session", token, { httpOnly: true, secure: true });

// Clear a cookie
return new Response(null).status(204).clearCookie("session");
```

---

## Error handling

```ts
import { HttpError } from "sky-framework/runtime";

throw new HttpError(404, "Item not found");
throw new HttpError(422, "Validation failed");
```

The gateway maps the error frame to the appropriate HTTP response.

---

## Middleware

### Per-handler (decorator)

```ts
import { requireAuth, cors, rateLimit } from "sky-framework/runtime";

@Handler({
  method: "GET",
  path: "/admin/report",
  middleware: [requireAuth(), cors({ origins: ["https://example.com"] })],
})
async report() { ... }
```

### Built-in middleware

| Export | Description |
|---|---|
| `requireAuth()` | Requires a valid JWT (verified by gateway); injects `claims` into `RequestContext` |
| `cors(options?)` | Adds CORS headers; `options.origins` defaults to `["*"]` |
| `rateLimit(options)` | Token-bucket rate limiting (gateway-side config via `sky.toml`) |

### JWT helpers (worker-side)

```ts
import { issueToken, verifyToken } from "sky-framework/runtime";

const token = await issueToken({ sub: userId, role: "admin" });
const claims = await verifyToken(token);   // throws if invalid or expired
```

---

## `sky` CLI

The `sky` binary is provided by this package (bin: `sky`).

```bash
sky build                      # Emit sky-manifest.json (one-shot)
sky build --watch              # Watch mode — rebuilds on any .ts change in source dirs
sky build -c path/to/sky.toml  # Use a non-default config location
```

`sky build` reads `[build].sources` from `sky.toml`, imports each discovered `.ts` file to trigger decorator registration, runs the type walker over the registered metadata, and writes `sky-manifest.json` to `manifest_path`.

Watch mode uses a 200ms debounce and re-resolves entry points from `sky.toml` on each rebuild so newly added or deleted service files are picked up automatically.

---

## Development

```bash
bun install        # Install dependencies
bun run typecheck  # Type-check without emitting
bun test           # Run tests
```

---

## Dependencies

| Package | Purpose |
|---|---|
| `@blue.ts/di` | Scoped and singleton DI container |
| `@msgpack/msgpack` | MessagePack encode/decode for the Sky framing transport |
| `busboy` | Multipart form-data parsing |
| `jose` | JWT signing and verification |
| `pino` | Structured logging |
| `zod` | Schema validation for `ZodBody` |
