import type { SkyInvocation } from "../../transport";
import type {CorsConfig} from "../middleware/cors";
import type {RateLimitConfig} from "../middleware/rateLimit";

export interface NativeMiddlewareDescriptor {
    kind: "native";
    name: string;
    global: boolean;
    order: number;
    config: CorsConfig | RateLimitConfig;
}

export interface MiddlewareContext {
    invocation: SkyInvocation;
    handlerId: string;
    method: string;
    path: string;
    params: Record<string, string>;
    query: Record<string, string>;
    headers: Record<string, string>;
}

export type SkyBody = object | AsyncGenerator<Uint8Array>;

export interface SkyResponse {
    status: number;
    headers: Record<string, string>;
    body: SkyBody;
}

export interface SkyMiddleware {
    handle(ctx: MiddlewareContext, next: () => Promise<SkyResponse>): Promise<SkyResponse>;
}
