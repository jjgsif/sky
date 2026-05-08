import { SkyWorkerSocket, type SkyInvocation } from "@sky/transport";
import { ServiceRegistry } from "./service-registry.js";
import type { SkyMiddleware, SkyResponse, SkyBody, MiddlewareContext } from "./middleware/types";
import { HttpError } from "./errors";

const manifest = await import(`${process.cwd()}/sky-manifest.json`, { with: { type: 'json' } });

// ---------------------------------------------------------------------------
// Manifest handler metadata index
// ---------------------------------------------------------------------------

interface MiddlewareEntry {
  kind: "native" | "user";
  name: string;
}

interface HandlerMeta {
  status: number;
  validate: boolean;
  // Full ordered chain — native entries are hoisted to the gateway; only "user" entries run here.
  middleware: MiddlewareEntry[];
}

// Index status, validate flag, and middleware chain per handler.
// Extract descriptors come from ServiceRegistry (source of truth is the decorated source).
const handlerMeta = new Map<string, HandlerMeta>();

for (const service of manifest.services) {
  const serviceMiddleware: MiddlewareEntry[] = (service.middleware ?? []) as MiddlewareEntry[];
  for (const handler of service.handlers) {
    handlerMeta.set(`${service.name}.${handler.name}`, {
      status: handler.status,
      validate: handler.validate,
      middleware: [...serviceMiddleware, ...((handler.middleware ?? []) as MiddlewareEntry[])],
    });
  }
}

// ---------------------------------------------------------------------------
// HandlerDispatcher — used by server.ts
// ---------------------------------------------------------------------------

export interface HandlerDispatcher {
  /**
   * Start the invocation loop against the given socket.
   * Returns a stop function that signals the dispatcher to stop
   * accepting new invocations after in-flight requests complete.
   */
  start(socket: SkyWorkerSocket): () => void;
}

export async function createDispatcher(
  registry: ServiceRegistry,
  middlewareMap: Map<string, SkyMiddleware> = new Map(),
): Promise<HandlerDispatcher> {
  return {
    start(socket: SkyWorkerSocket): () => void {
      let stopped = false;

      void (async () => {
        for await (const invocation of socket.invocations()) {
          if (stopped) break;

          handleInvocation(socket, registry, middlewareMap, invocation).catch((err: Error) => {
            console.error(`[sky/worker] unhandled error in ${invocation.handlerId}:`, err);
            socket.sendError(
              invocation.requestId,
              "UNHANDLED_EXCEPTION",
              err.message,
              err.stack,
            );
          });
        }
      })();

      return () => { stopped = true; };
    },
  };
}

// ---------------------------------------------------------------------------
// Per-request handler
// ---------------------------------------------------------------------------

async function handleInvocation(
  socket: SkyWorkerSocket,
  registry: ServiceRegistry,
  middlewareMap: Map<string, SkyMiddleware>,
  invocation: SkyInvocation,
): Promise<void> {
  const [serviceName, methodName] = invocation.handlerId.split(".");

  const serviceInfo = registry.getServiceInfoByClassName(serviceName);
  if (!serviceInfo) {
    socket.sendError(
      invocation.requestId,
      "HANDLER_NOT_FOUND",
      `No service registered for ${serviceName}`,
    );
    return;
  }

  const meta = handlerMeta.get(invocation.handlerId);
  if (!meta) {
    socket.sendError(
      invocation.requestId,
      "MANIFEST_MISMATCH",
      `No manifest entry for ${invocation.handlerId}`,
    );
    return;
  }

  // Extract descriptors come from the registry — source of truth is the
  // decorated source file, not the manifest. Position is implied by array index.
  const extracts = registry.getExtracts(serviceInfo.name, methodName);

  // Fresh scope per request — scoped services get new instances,
  // singletons resolve from root container automatically
  const scope = registry.container.createScope();
  const service = await scope.get(serviceInfo.cls) as Record<string, Function>;
  const handler = service[methodName].bind(service);

  const args = extracts.map((descriptor) => {
    switch (descriptor.source) {
      case "body":
        return descriptor.stream
          ? invocation.body                   // AsyncGenerator<Uint8Array>
          : deserializeBody(invocation.body); // validated, deserialized object
      case "query":
        return invocation.query[descriptor.name!];
      case "param":
        return invocation.params[descriptor.name!];
      case "header":
        return invocation.headers[descriptor.name!.toLowerCase()];
    }
  });

  // Build middleware chain — native entries (CORS etc.) are hoisted to the gateway; skip them here.
  const chain: SkyMiddleware[] = meta.middleware
    .filter(m => m.kind === "user")
    .map(m => middlewareMap.get(m.name))
    .filter((m): m is SkyMiddleware => m !== undefined);

  const ctx: MiddlewareContext = {
    invocation,
    handlerId: invocation.handlerId,
    method: invocation.method,
    path: invocation.path,
    params: invocation.params,
    query: invocation.query,
    headers: invocation.headers,
  };

  const leaf = async (): Promise<SkyResponse> => {
    const result = await handler(...args);
    return normalize(result, meta.status);
  };

  try {
    const response = await runMiddlewareChain(chain, ctx, leaf);
    await sendResponse(socket, invocation.requestId, response);
  } catch (err) {
    if (err instanceof HttpError) {
      await sendHttpErrorResponse(socket, invocation.requestId, err);
    } else {
      throw err;
    }
  }
}

// ---------------------------------------------------------------------------
// HttpError response (sends a real HTTP response with the specified status)
// ---------------------------------------------------------------------------

async function sendHttpErrorResponse(
  socket: SkyWorkerSocket,
  requestId: number,
  err: HttpError,
): Promise<void> {
  const body = JSON.stringify({ code: "HTTP_ERROR", message: err.message });
  const bytes = new TextEncoder().encode(body);
  socket.sendHead(requestId, err.status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": String(bytes.length),
  });
  socket.sendChunk(requestId, bytes);
  socket.sendEnd(requestId);
}

// ---------------------------------------------------------------------------
// Middleware chain execution
// ---------------------------------------------------------------------------

async function runMiddlewareChain(
  chain: SkyMiddleware[],
  ctx: MiddlewareContext,
  leaf: () => Promise<SkyResponse>,
): Promise<SkyResponse> {
  if (chain.length === 0) return leaf();
  return chain[0].handle(ctx, () => runMiddlewareChain(chain.slice(1), ctx, leaf));
}

// ---------------------------------------------------------------------------
// Response serialization
// ---------------------------------------------------------------------------

async function sendResponse(
  socket: SkyWorkerSocket,
  requestId: number,
  response: SkyResponse,
): Promise<void> {
  if (isAsyncGenerator(response.body)) {
    socket.sendHead(requestId, response.status, response.headers);
    for await (const chunk of response.body) {
      socket.sendChunk(requestId, chunk);
    }
    socket.sendEnd(requestId);
  } else {
    const encoder = new TextEncoder();
    const bytes = encoder.encode(JSON.stringify(response.body));
    socket.sendHead(requestId, response.status, {
      "content-type": "application/json; charset=utf-8",
      "content-length": String(bytes.length),
      ...response.headers,
    });
    socket.sendChunk(requestId, bytes);
    socket.sendEnd(requestId);
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function normalize(result: unknown, defaultStatus: number): SkyResponse {
  if (result !== null && typeof result === "object" && "body" in result) {
    const r = result as { status?: number; headers?: Record<string, string>; body: SkyBody };
    return {
      status: r.status ?? defaultStatus,
      headers: r.headers ?? {},
      body: r.body,
    };
  }
  return {
    status: defaultStatus,
    headers: {},
    body: result as object,
  };
}

function isAsyncGenerator(val: unknown): val is AsyncGenerator<Uint8Array> {
  return (
    val !== null &&
    typeof val === "object" &&
    typeof (val as AsyncGenerator)[Symbol.asyncIterator] === "function"
  );
}

function deserializeBody(body: Uint8Array): unknown {
  if (body.length === 0) return undefined;
  return JSON.parse(new TextDecoder().decode(body));
}
