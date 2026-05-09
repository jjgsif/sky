import { describe, test, expect, beforeAll } from "bun:test";
import { buildRequestContext } from "@sky/runtime/context";
import { assembleManifest, type ManifestOutput } from "@sky/emitter";
import path from "path";

// ── buildRequestContext unit tests ────────────────────────────────────────────

function makeInvocation(overrides: Partial<{
    requestId: number;
    handlerId: string;
    method: string;
    path: string;
    params: Record<string, string>;
    query: Record<string, string>;
    headers: Record<string, string>;
    body: Uint8Array;
}> = {}) {
    return {
        requestId: overrides.requestId ?? 42,
        handlerId: overrides.handlerId ?? "UserService.getMe",
        method:    overrides.method    ?? "GET",
        path:      overrides.path      ?? "/me",
        params:    overrides.params    ?? {},
        query:     overrides.query     ?? {},
        headers:   overrides.headers   ?? {},
        body:      overrides.body      ?? new Uint8Array(0),
    };
}

describe("buildRequestContext", () => {
    test("populates scalar fields from invocation", () => {
        const inv = makeInvocation({ requestId: 7, method: "POST", path: "/items" });
        const ctx = buildRequestContext(inv);
        expect(ctx.requestId).toBe(7);
        expect(ctx.method).toBe("POST");
        expect(ctx.path).toBe("/items");
        expect(ctx.handlerId).toBe("UserService.getMe");
    });

    test("passes through params, query, headers maps", () => {
        const inv = makeInvocation({
            params:  { id: "123" },
            query:   { page: "2" },
            headers: { "content-type": "application/json" },
        });
        const ctx = buildRequestContext(inv);
        expect(ctx.params).toEqual({ id: "123" });
        expect(ctx.query).toEqual({ page: "2" });
        expect(ctx.headers["content-type"]).toBe("application/json");
    });

    test("claims is null when x-sky-claims header is absent", () => {
        const ctx = buildRequestContext(makeInvocation());
        expect(ctx.claims).toBeNull();
    });

    test("claims is parsed from x-sky-claims header", () => {
        const payload = { sub: "user-42", role: "admin", iat: 1000, exp: 9999 };
        const inv = makeInvocation({
            headers: { "x-sky-claims": JSON.stringify(payload) },
        });
        const ctx = buildRequestContext(inv);
        expect(ctx.claims).toEqual(payload);
        expect(ctx.claims?.["sub"]).toBe("user-42");
        expect(ctx.claims?.["role"]).toBe("admin");
    });

    test("claims is null when x-sky-claims is invalid JSON", () => {
        const inv = makeInvocation({
            headers: { "x-sky-claims": "not-valid-json" },
        });
        const ctx = buildRequestContext(inv);
        expect(ctx.claims).toBeNull();
    });

    test("body is undefined for empty payload", () => {
        const ctx = buildRequestContext(makeInvocation({ body: new Uint8Array(0) }));
        expect(ctx.body).toBeUndefined();
    });

    test("body is deserialized from JSON bytes", () => {
        const payload = { name: "Widget", price: 9.99 };
        const bytes = new TextEncoder().encode(JSON.stringify(payload));
        const ctx = buildRequestContext(makeInvocation({ body: bytes }));
        expect(ctx.body).toEqual(payload);
    });
});

// ── Assembler: Context() descriptors must not appear in manifest ─────────────

const FIXTURE = path.join(import.meta.dir, "fixtures", "context-service.ts");
let manifest: ManifestOutput;

beforeAll(async () => {
    manifest = await assembleManifest(
        [FIXTURE],
        path.join(import.meta.dir, "fixtures", "context-service-manifest.json"),
    );
});

describe("assembler — Context() descriptor handling", () => {
    test("ContextService is discovered", () => {
        const svc = manifest.services.find(s => s.name === "ContextService");
        expect(svc).toBeDefined();
    });

    test("no extract entry has source === 'context'", () => {
        const allExtracts = manifest.services.flatMap(s =>
            s.handlers.flatMap(h => h.extract),
        );
        // @ts-ignore
        const contextEntries = allExtracts.filter(e => e.source === "context");
        expect(contextEntries).toHaveLength(0);
    });

    test("getMe handler has zero extracts (ctx only, skipped)", () => {
        const svc = manifest.services.find(s => s.name === "ContextService");
        const handler = svc!.handlers.find(h => h.name === "getMe");
        expect(handler).toBeDefined();
        expect(handler!.extract).toHaveLength(0);
    });

    test("createItem handler has zero extracts (ctx with body type, skipped)", () => {
        const svc = manifest.services.find(s => s.name === "ContextService");
        const handler = svc!.handlers.find(h => h.name === "createItem");
        expect(handler!.extract).toHaveLength(0);
    });

    test("getItem handler has only the Param extract, not ctx", () => {
        const svc = manifest.services.find(s => s.name === "ContextService");
        const handler = svc!.handlers.find(h => h.name === "getItem");
        expect(handler!.extract).toHaveLength(1);
        expect(handler!.extract[0].source).toBe("param");
        expect(handler!.extract[0].name).toBe("id");
    });
});
