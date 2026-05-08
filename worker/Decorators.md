# Sky Decorators Reference

This document catalogs every decorator and extractor exported from `sky/decorators`,
along with their TypeScript types and a usage example.

Decorators rely on the **TC39 Stage 3 decorators** form (`ClassDecoratorContext`,
`ClassMethodDecoratorContext`) — not the legacy `experimentalDecorators` flag.
Metadata is stored on `context.metadata` and read by the build-time emitter to
produce `sky-manifest.json`.

```ts
import {
    Service,
    Handler,
    Group,
    Middleware,
    Body,
    StreamedBody,
    Param,
    Query,
    Header,
    ZodBody,
} from "sky/decorators";
```

---

## Class Decorators

### `@Service`

Marks a class as a Sky service. The emitter walks every `@Service`-decorated class
to discover handlers, then registers it with the DI container at runtime.

**Type**

```ts
interface ServiceOptions {
    lifetime?: "singleton" | "scoped" | "transient"; // default: "scoped"
    name?: string;
    dependencies?: (string | Function | symbol)[];
}

function Service(options?: ServiceOptions):
    (value: Function, context: ClassDecoratorContext) => void;
```

| Lifetime    | Behavior                                                                 |
|-------------|--------------------------------------------------------------------------|
| `singleton` | One instance for the lifetime of the worker.                             |
| `scoped`    | One instance per request (default — gets a fresh DI scope).              |
| `transient` | New instance every resolution.                                           |

**Example**

```ts
import { Service, Handler, Body } from "sky/decorators";

@Service({ lifetime: "scoped" })
class UserService {
    @Handler({ method: "POST", path: "/users", extract: [Body()] })
    async create(body: { name: string }) {
        return { id: crypto.randomUUID(), name: body.name };
    }
}
```

---

### `@Group`

Adds a URL prefix (and optional middleware) to every handler on the class.
Routes are emitted as `prefix + handler.path`.

**Type**

```ts
interface GroupOptions {
    prefix: string;
    middleware?: Function[];
}

function Group(options: GroupOptions):
    (value: Function, context: ClassDecoratorContext) => void;
```

**Example**

```ts
@Service()
@Group({ prefix: "/api/admin" })
class AdminService {
    // emitted route: GET /api/admin/users
    @Handler({ method: "GET", path: "/users" })
    async list() {
        return [];
    }
}
```

---

### `@Middleware`

Two call shapes:

1. `@Middleware(SomeMiddlewareClass)` — apply that middleware to every handler on
   this service.
2. `@Middleware({ global, order })` — declare *this* class as a middleware
   provider, with optional global registration and ordering.

**Type**

```ts
interface MiddlewareOptions {
    global?: boolean;       // default: false
    order?: number;         // default: 0
}

function Middleware(arg?: Function | MiddlewareOptions):
    (value: Function, context: ClassDecoratorContext) => void;
```

**Example — apply middleware to a service**

```ts
@Service()
@Middleware(AuthMiddleware)         // runs before every handler in OrdersService
class OrdersService {
    @Handler({ method: "GET", path: "/orders" })
    async list() { /* ... */ }
}
```

**Example — declare a middleware class**

```ts
@Middleware({ global: true, order: -100 })
class RequestLoggerMiddleware {
    async handle(req: Request, next: () => Promise<Response>) {
        const started = performance.now();
        const res = await next();
        console.log(req.method, req.url, performance.now() - started, "ms");
        return res;
    }
}
```

---

## Method Decorators

### `@Handler`

Registers a method as an HTTP handler. The `extract` array is positional:
`extract[i]` is fed to parameter `i` of the method.

**Type**

```ts
type HttpMethod = "GET" | "POST" | "PATCH" | "PUT" | "DELETE";

interface InlineNativeMiddleware {
    kind: "native";
    name: string;
    config?: unknown;
}

interface HandlerOptions {
    path: string;
    method: HttpMethod;
    status?: number;        // default: GET→200, POST→201, PUT/PATCH→200, DELETE→204
    extract?: ExtractDescriptor[];
    validate?: boolean;     // default: true
    streaming?: boolean;    // default: false — see "Streaming responses" below
    middleware?: (Function | InlineNativeMiddleware)[];
}

function Handler(options: HandlerOptions):
    (value: Function, context: ClassMethodDecoratorContext) => void;
```

**Example**

```ts
@Service()
class ItemService {
    @Handler({
        method: "POST",
        path: "/items",
        status: 201,
        extract: [Body(), Header("x-tenant-id")],
    })
    async create(body: { name: string }, tenantId: string | undefined) {
        return { id: 1, name: body.name, tenantId };
    }
}
```

### Streaming responses

Set `streaming: true` to send the response body to the HTTP client chunk-by-chunk
via HTTP/1.1 chunked transfer encoding, instead of buffering it in the gateway
until the worker finishes. The handler must return an `AsyncGenerator<Uint8Array>`
(or a wrapper that exposes one as `body`); each `yield` becomes a wire chunk
forwarded to the client immediately, lowering TTFB and allowing arbitrarily
large responses (NDJSON streams, file downloads, SSE) without proportional
gateway memory.

```ts
@Service({ lifetime: "singleton" })
class StreamService {
    @Handler({
        method: "GET",
        path: "/stream/events",
        streaming: true,
        extract: [Query("count")],
    })
    async events(count?: string) {
        const n = Math.min(parseInt(count ?? "10", 10), 100);
        return {
            status: 200,
            headers: { "content-type": "application/x-ndjson" },
            body: generate(n),
        };
    }
}

async function* generate(n: number): AsyncGenerator<Uint8Array> {
    const enc = new TextEncoder();
    for (let i = 1; i <= n; i++) {
        yield enc.encode(JSON.stringify({ seq: i }) + "\n");
    }
}
```

Backpressure is enforced by a bounded channel between the worker and the
gateway's response stream, so a slow client throttles the producer rather
than ballooning gateway memory.

Leave `streaming: false` (the default) for unary JSON responses — the buffered
path has slightly lower per-request overhead and emits a `Content-Length`
header.

---

## Extract Descriptors

These are factory functions, not decorators — they appear inside the
`@Handler({ extract: [...] })` array. Each one returns a descriptor object that
the emitter encodes into the manifest, and the dispatcher uses at runtime to
build the argument list.

```ts
type ExtractDescriptor =
    | BodyDescriptor
    | HeaderDescriptor
    | QueryDescriptor
    | ParamDescriptor;

interface BodyDescriptor   { source: "body";   stream: boolean }
interface HeaderDescriptor { source: "header"; name: string }
interface QueryDescriptor  { source: "query";  name: string }
interface ParamDescriptor  { source: "param";  name: string }
```

### `Body()`

Parses the request body as JSON and passes the resulting object.

```ts
function Body(): BodyDescriptor;   // { source: "body", stream: false }
```

```ts
@Handler({ method: "POST", path: "/items", extract: [Body()] })
async create(body: { name: string; price: number }) { /* ... */ }
```

### `StreamedBody()`

Marks the body as streaming — the body is delivered as bytes without being
buffered or JSON-parsed up front.

```ts
function StreamedBody(): BodyDescriptor;  // { source: "body", stream: true }
```

```ts
@Handler({ method: "POST", path: "/upload", extract: [StreamedBody()] })
async upload(stream: ReadableStream<Uint8Array>) { /* ... */ }
```

### `Param(name)`

A typed URL path parameter (e.g. `:id` in `/users/:id`).

```ts
function Param(name: string): ParamDescriptor;
```

```ts
@Handler({ method: "GET", path: "/users/:id", extract: [Param("id")] })
async getById(id: string) { /* ... */ }
```

### `Query(name)`

A query-string parameter. May be `undefined` if not provided.

```ts
function Query(name: string): QueryDescriptor;
```

```ts
@Handler({ method: "GET", path: "/search", extract: [Query("q"), Query("limit")] })
async search(q: string | undefined, limit: string | undefined) { /* ... */ }
```

### `Header(name)`

A request header (lowercased name). May be `undefined` if not present.

```ts
function Header(name: string): HeaderDescriptor;
```

```ts
@Handler({ method: "GET", path: "/me", extract: [Header("authorization")] })
async me(auth: string | undefined) { /* ... */ }
```

### `ZodBody(schema)`

Like `Body()`, but additionally records the body's Zod schema so the emitter can
produce JSON Schema for gateway-side validation. The handler parameter is
inferred via `z.infer<typeof schema>`.

**Type**

```ts
import type { ZodType, toJSONSchema } from "zod";

interface ZodBodyDescriptor extends BodyDescriptor {
    schema: ZodType;
    jsonSchema: ReturnType<typeof toJSONSchema<ZodType>>;
}

function ZodBody(schema: ZodType): ZodBodyDescriptor;
```

**Example**

```ts
import { z } from "zod";

const CreateUser = z.object({
    name: z.string().min(1),
    email: z.string().email(),
});

@Service()
class UserService {
    @Handler({ method: "POST", path: "/users", extract: [ZodBody(CreateUser)] })
    async create(body: z.infer<typeof CreateUser>) {
        return { id: crypto.randomUUID(), ...body };
    }
}
```

When `validate: true` (the default), the gateway rejects requests that don't
match the schema with a `400 Bad Request` *before* the worker is invoked.

---

## Putting it all together

```ts
import { Service, Handler, Group, Middleware, Param, Query, Header, ZodBody } from "sky/decorators";
import { Response, HttpError } from "sky/runtime";
import { z } from "zod";

const CreateOrder = z.object({
    sku: z.string(),
    quantity: z.number().int().positive(),
});

@Service({ lifetime: "scoped" })
@Group({ prefix: "/api/orders" })
@Middleware(AuthMiddleware)
class OrderService {
    @Handler({ method: "GET", path: "/:id", extract: [Param("id"), Header("x-tenant-id")] })
    async getById(id: string, tenantId: string | undefined) {
        if (!tenantId) throw new HttpError(400, "missing tenant");
        return { id, tenantId };
    }

    @Handler({ method: "POST", path: "/", extract: [ZodBody(CreateOrder)] })
    async create(body: z.infer<typeof CreateOrder>) {
        return new Response({ id: crypto.randomUUID(), ...body }).status(201);
    }

    @Handler({ method: "GET", path: "/", extract: [Query("status")] })
    async list(status: string | undefined) {
        return { status: status ?? "all", items: [] };
    }
}
```
