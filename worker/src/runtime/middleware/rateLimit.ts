import type {InlineNativeMiddleware} from "../../decorators";

/**
 * Identifies the source of a request for rate limiting purposes.
 *
 * - `"ip"` — use the client's IP address (from `x-forwarded-for` or the socket remote address)
 * - `"headers:<name>"` — use the value of a specific request header (e.g. `"headers:x-api-key"`)
 * - any other string — a custom static key; all requests share the same bucket regardless of origin
 */
export type Identifier = "ip" | `headers:${string}` | (string & {});

/**
 * Configuration for the `rateLimit` middleware.
 *
 * @example
 * // 100 requests per minute per IP, shared across all /api routes
 * @Handler({ method: "GET", path: "/api/items", middleware: [rateLimit({ bucket: "api", perMinute: 100, identifier: "ip" })] })
 *
 * @example
 * // 50 requests per minute keyed by API key header
 * rateLimit({ bucket: "api-key-tier", perMinute: 50, identifier: "headers:x-api-key" })
 */
export interface RateLimitConfig {
    /**
     * Logical name for the counter bucket. Requests with the same bucket name and identifier
     * key share a counter, allowing multiple routes to contribute to a single limit.
     */
    bucket: string;
    /** Maximum number of requests allowed within a 60-second sliding window. */
    perMinute: number;
    /** How to derive the per-client key from the incoming request. */
    identifier: Identifier;
}

/**
 * Declares rate-limit middleware for a handler or service.
 *
 * Pass the returned value to `@Handler({ middleware: [...] })` or `@Service({ middleware: [...] })`.
 * Requests exceeding the limit receive a `429 Too Many Requests` response before the handler runs.
 *
 * Counters are maintained in the Rust gateway runtime — buckets are keys in a `DashMap` updated
 * on every dispatched request, so the limit applies consistently across all workers in the pool.
 */
export function rateLimit(options: RateLimitConfig): InlineNativeMiddleware<RateLimitConfig> {
    return {
        name: "rateLimit",
        kind: "native",
        config: options
    };
}