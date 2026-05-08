import type { NativeMiddlewareDescriptor } from "../runtime/middleware/types";

const registry: NativeMiddlewareDescriptor[] = [];

export function globalMiddleware(ms: NativeMiddlewareDescriptor[]): void {
    registry.push(...ms);
}

export function getNativeMiddleware(): NativeMiddlewareDescriptor[] {
    return registry;
}
