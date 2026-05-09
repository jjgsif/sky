import type { InlineNativeMiddleware } from "../../decorators";

/**
 * Configuration for the `requireAuth` middleware.
 *
 * @example
 * // Require any valid JWT
 * @Handler({ method: "GET", path: "/me", middleware: [requireAuth()] })
 *
 * @example
 * // Require specific OAuth scopes
 * @Handler({ method: "DELETE", path: "/admin/users/:id", middleware: [requireAuth({ scopes: ["admin"] })] })
 *
 * @example
 * // Attempt auth but allow unauthenticated access
 * @Handler({ method: "GET", path: "/feed", middleware: [requireAuth({ optional: true })] })
 */
export interface AuthConfig {
    /**
     * OAuth-style scopes the JWT must include (either as a space-separated
     * `scope` string or a `scopes` array in the token payload).
     * Empty array means any valid token is accepted.
     */
    scopes?: string[];
    /**
     * When true, requests without a token (or with an invalid token) are still
     * forwarded to the handler — no `x-sky-claims` header is injected. Useful
     * for routes that behave differently for authenticated vs anonymous users.
     */
    optional?: boolean;
}

/**
 * Declares gateway-level JWT authentication for a handler or service.
 *
 * Pass the returned value to `@Handler({ middleware: [...] })` or apply
 * `@Middleware(requireAuth())` to a service class to protect all its handlers.
 *
 * The gateway verifies the token against `[auth].jwt_secret` in `sky.toml`
 * **before** forwarding the request to the worker pool. Verified claims are
 * forwarded as the `x-sky-claims` request header (JSON-encoded).
 *
 * Use `issueToken()` from `sky/runtime` in a login handler to produce tokens.
 */
export function requireAuth(config?: AuthConfig): InlineNativeMiddleware<AuthConfig> {
    return { name: "auth", kind: "native", config: config ?? {} };
}
