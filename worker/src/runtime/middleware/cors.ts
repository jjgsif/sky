import type { NativeMiddlewareDescriptor } from "./types";

export interface CorsConfig {
    /** Allowed origins. Use `["*"]` to allow any origin. */
    origins: string[];
    /** Whether to allow credentials (cookies, auth headers). Default: false. */
    credentials?: boolean;
    /** Preflight cache duration in seconds. Default: 86400. */
    maxAge?: number;
    /** Allowed request headers. Default: ["content-type", "authorization"]. */
    allowHeaders?: string[];
    /** Response headers exposed to the browser. Default: []. */
    exposeHeaders?: string[];
    /** Allowed HTTP methods. Default: GET, POST, PUT, PATCH, DELETE, OPTIONS, HEAD. */
    allowMethods?: string[];
}

export function cors(config: CorsConfig): NativeMiddlewareDescriptor {
    return {
        kind: "native",
        name: "cors",
        global: true,
        order: -1000,
        config,
    };
}
