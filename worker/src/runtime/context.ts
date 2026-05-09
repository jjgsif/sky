import type { SkyInvocation } from "../transport";

export interface RequestContext<TBody = unknown> {
    requestId:  number;
    handlerId:  string;
    method:     string;
    path:       string;
    params:     Record<string, string>;
    query:      Record<string, string>;
    headers:    Record<string, string>;
    /** Parsed JWT claims forwarded by the gateway after verification. Null on unauthenticated routes. */
    claims:     Record<string, unknown> | null;
    /** Deserialized request body. TBody is a compile-time cast — use @ZodBody for schema validation. */
    body:       TBody;
}

export function buildRequestContext(invocation: SkyInvocation): RequestContext {
    const rawClaims = invocation.headers["x-sky-claims"];
    let claims: Record<string, unknown> | null = null;
    if (rawClaims) {
        try { claims = JSON.parse(rawClaims) as Record<string, unknown>; } catch { /* invalid JSON — treat as no claims */ }
    }

    const body = invocation.body.length > 0
        ? JSON.parse(new TextDecoder().decode(invocation.body)) as unknown
        : undefined;

    return {
        requestId: invocation.requestId,
        handlerId: invocation.handlerId,
        method:    invocation.method,
        path:      invocation.path,
        params:    invocation.params,
        query:     invocation.query,
        headers:   invocation.headers,
        claims,
        body,
    };
}
