import Busboy from "busboy";
import { Readable } from "node:stream";
import { createWriteStream } from "node:fs";
import { pipeline } from "node:stream/promises";

export interface UploadPart {
    field: string;
    filename?: string;
    mimeType: string;
    /** Buffer the entire part into memory. */
    bytes(): Promise<Uint8Array>;
    /** Stream the part directly to a file path. */
    saveTo(path: string): Promise<void>;
}

interface PendingPart {
    part: UploadPart;
    /** Resolves when the inner file stream has been fully consumed or drained. */
    drained: Promise<void>;
    /** Call to put the inner stream into flowing/discard mode. */
    drain(): void;
}

/**
 * Parse a `multipart/form-data` stream yielded by `StreamedBody()`.
 *
 * Yields one `UploadPart` per boundary section. For each part, call either
 * `bytes()` to buffer it in memory or `saveTo(path)` to write it to disk.
 * If neither is called, the part is auto-drained before the next part is
 * yielded, so busboy never stalls.
 *
 * @param stream      The `AsyncIterable<Uint8Array>` from `StreamedBody()`.
 * @param contentType The `Content-Type` header value (must include boundary).
 */
export async function* parseMultipart(
    stream: AsyncIterable<Uint8Array>,
    contentType: string,
): AsyncGenerator<UploadPart> {
    const bb = Busboy({ headers: { "content-type": contentType } });

    const queue: PendingPart[] = [];
    let bbFinished = false;
    let bbError: unknown = null;

    // Single-slot resolver: unblocks the generator loop when a new part arrives,
    // busboy finishes, or an error occurs.
    let notify: (() => void) | null = null;
    const waitForActivity = () => new Promise<void>(r => { notify = r; });
    const signal = () => { notify?.(); notify = null; };

    bb.on("file", (field, fileStream, info) => {
        let consumed = false;
        let drainResolve!: () => void;
        const drained = new Promise<void>(r => { drainResolve = r; });

        fileStream.on("end", drainResolve);

        const part: UploadPart = {
            field,
            filename: info.filename || undefined,
            mimeType: info.mimeType,

            async bytes(): Promise<Uint8Array> {
                consumed = true;
                const chunks: Buffer[] = [];
                for await (const chunk of fileStream) {
                    chunks.push(chunk as Buffer);
                }
                return new Uint8Array(Buffer.concat(chunks));
            },

            async saveTo(path: string): Promise<void> {
                consumed = true;
                await pipeline(fileStream, createWriteStream(path));
            },
        };

        queue.push({
            part,
            drained,
            drain() {
                if (!consumed) fileStream.resume();
            },
        });

        signal();
    });

    // Plain form fields (no filename in Content-Disposition) come through "field",
    // not "file". Busboy buffers the entire value synchronously before firing.
    bb.on("field", (field, value, info) => {
        const encoded = new TextEncoder().encode(value);

        const part: UploadPart = {
            field,
            filename: undefined,
            mimeType: info.mimeType,

            async bytes(): Promise<Uint8Array> {
                return encoded;
            },

            async saveTo(path: string): Promise<void> {
                await Bun.write(path, encoded);
            },
        };

        // Field values are already buffered — nothing to drain.
        queue.push({ part, drained: Promise.resolve(), drain() {} });
        signal();
    });

    bb.on("finish", () => { bbFinished = true; signal(); });
    bb.on("error", (err) => { bbError = err; signal(); });

    // Bridge AsyncIterable<Uint8Array> → Node Readable → busboy
    Readable.from(stream).pipe(bb);

    let prev: PendingPart | null = null;

    while (true) {
        // Ensure the previous part's data is consumed before moving on.
        if (prev) {
            prev.drain();
            await prev.drained;
            prev = null;
        }

        // Wait until there is something in the queue, or busboy is done.
        while (queue.length === 0 && !bbFinished && !bbError) {
            await waitForActivity();
        }

        if (bbError) throw bbError;
        if (queue.length === 0) break;

        const pending = queue.shift()!;
        prev = pending;
        yield pending.part;
    }
}
