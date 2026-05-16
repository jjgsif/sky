import { describe, test, expect, afterEach } from "bun:test";
import { parseMultipart } from "@sky/runtime/multipart";
import fs from "node:fs";
import path from "node:path";

// ── Helpers ────────────────────────────────────────────────────────────────────

const BOUNDARY = "TestBoundary1234";

interface PartSpec {
    field: string;
    filename?: string;
    mimeType?: string;
    data: string;
}

function makeMultipart(parts: PartSpec[]): {
    body: AsyncIterable<Uint8Array>;
    contentType: string;
} {
    const lines: string[] = [];
    for (const p of parts) {
        lines.push(`--${BOUNDARY}`);
        const disposition = p.filename
            ? `Content-Disposition: form-data; name="${p.field}"; filename="${p.filename}"`
            : `Content-Disposition: form-data; name="${p.field}"`;
        lines.push(disposition);
        if (p.mimeType) lines.push(`Content-Type: ${p.mimeType}`);
        lines.push("");
        lines.push(p.data);
    }
    lines.push(`--${BOUNDARY}--`);

    const raw = lines.join("\r\n");
    const bytes = new TextEncoder().encode(raw);

    const body: AsyncIterable<Uint8Array> = {
        [Symbol.asyncIterator]: async function* () { yield bytes; },
    };

    return {
        body,
        contentType: `multipart/form-data; boundary=${BOUNDARY}`,
    };
}

// ── Cleanup ────────────────────────────────────────────────────────────────────

const tmpFiles: string[] = [];
afterEach(() => {
    for (const f of tmpFiles) {
        try { fs.unlinkSync(f); } catch {}
    }
    tmpFiles.length = 0;
});

function tmpPath(name: string): string {
    const p = path.join("/tmp", `sky-multipart-test-${name}-${Date.now()}`);
    tmpFiles.push(p);
    return p;
}

// ── Tests ──────────────────────────────────────────────────────────────────────

describe("parseMultipart", () => {

    test("single file part: bytes() and filename", async () => {
        const { body, contentType } = makeMultipart([
            { field: "file", filename: "hello.txt", mimeType: "text/plain", data: "hello world" },
        ]);

        const parts = [];
        for await (const part of parseMultipart(body, contentType)) {
            parts.push({ ...part, data: new TextDecoder().decode(await part.bytes()) });
        }

        expect(parts).toHaveLength(1);
        expect(parts[0]!.field).toBe("file");
        expect(parts[0]!.filename).toBe("hello.txt");
        expect(parts[0]!.mimeType).toBe("text/plain");
        expect(parts[0]!.data).toBe("hello world");
    });

    test("text field: no filename, bytes() returns value", async () => {
        const { body, contentType } = makeMultipart([
            { field: "username", data: "alice" },
        ]);

        const parts = [];
        for await (const part of parseMultipart(body, contentType)) {
            parts.push({ field: part.field, filename: part.filename, data: new TextDecoder().decode(await part.bytes()) });
        }

        expect(parts).toHaveLength(1);
        expect(parts[0]!.field).toBe("username");
        expect(parts[0]!.filename).toBeUndefined();
        expect(parts[0]!.data).toBe("alice");
    });

    test("multiple parts: yielded in order", async () => {
        const { body, contentType } = makeMultipart([
            { field: "first", data: "AAA" },
            { field: "second", data: "BBB" },
            { field: "third", data: "CCC" },
        ]);

        const results: string[] = [];
        for await (const part of parseMultipart(body, contentType)) {
            results.push(new TextDecoder().decode(await part.bytes()));
        }

        expect(results).toEqual(["AAA", "BBB", "CCC"]);
    });

    test("saveTo(): writes correct bytes to disk", async () => {
        const content = "file contents here";
        const { body, contentType } = makeMultipart([
            { field: "upload", filename: "data.txt", data: content },
        ]);

        const dest = tmpPath("saveTo");

        for await (const part of parseMultipart(body, contentType)) {
            await part.saveTo(dest);
        }

        expect(fs.readFileSync(dest, "utf-8")).toBe(content);
    });

    test("auto-drain: skipping a part does not stall subsequent parts", async () => {
        const { body, contentType } = makeMultipart([
            { field: "skip-me", data: "ignored data" },
            { field: "keep-me", data: "important" },
        ]);

        const results: Array<{ field: string; data: string }> = [];
        for await (const part of parseMultipart(body, contentType)) {
            if (part.field === "keep-me") {
                results.push({ field: part.field, data: new TextDecoder().decode(await part.bytes()) });
            }
            // "skip-me": intentionally call neither bytes() nor saveTo()
        }

        expect(results).toHaveLength(1);
        expect(results[0]!.field).toBe("keep-me");
        expect(results[0]!.data).toBe("important");
    });

    test("error: bad content-type throws", async () => {
        const { body } = makeMultipart([{ field: "f", data: "x" }]);

        let threw = false;
        try {
            for await (const _ of parseMultipart(body, "text/plain")) { /* nothing */ }
        } catch {
            threw = true;
        }

        expect(threw).toBe(true);
    });

});
