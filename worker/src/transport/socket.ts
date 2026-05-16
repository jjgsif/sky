// packages/transport/src/socket.ts
//
// Client-side implementation of the Sky framing protocol.
// Internal to the framework — developers never touch this directly.

import { encode, decode } from "@msgpack/msgpack";
import { createHmac } from 'node:crypto';
import {
    FrameType, HEADER_SIZE, PROTOCOL_VERSION,
    type FrameHeader, type InvocationResult,
    type InvocationWaiter, type SkyInvocation
} from "./types";
import { decodeHeader } from "./util";

export class SkyWorkerSocket {
    private socket!: ReturnType<typeof Bun.connect> extends Promise<infer S> ? S : never;
    private readBuf: Buffer = Buffer.allocUnsafe(64 * 1024);
    private sendBuf: Buffer = Buffer.allocUnsafe(64 * 1024);
    private readBufLen = 0;
    private draining: boolean = false;

    private activeBodyStreams = new Map<number, {
        waiters: Array<{
           resolve: (result: IteratorResult<Uint8Array>) => void;
           reject: (err: unknown) => void;
        }>;
        chunks: Uint8Array[];
        done: boolean;
    }>();

    // ── Outbound write backpressure ───────────────────────────────────────────
    // Frames waiting for the kernel send buffer to drain. Each entry may be a
    // partial frame if socket.write() accepted only some of its bytes.
    private writeQueue: Buffer[] = [];
    private drainWaiters: Array<() => void> = [];

    // ── Per-request response credit (gateway → worker flow control) ──────────
    // Tracks how many response-body bytes the gateway has authorised the
    // worker to send for each in-flight request. sendChunk() blocks until
    // there is enough credit, preventing the worker from flooding the socket.
    private responseCredits = new Map<number, {
        bytes: number;
        waiters: Array<() => void>;
    }>();

    // Decoded invocations not yet consumed by the dispatcher
    private queue: SkyInvocation[] = [];

    // Parked Promise resolve callbacks waiting for the next invocation
    private waiters: InvocationWaiter[] = [];

    // External lifecycle hooks — set by server.ts
    private drainedCallback: (() => void) | null = null;
    private socketClosedCallback: (() => void) | null = null;

    // Auth handshake state
    private _secret: Buffer | null = null;
    private _authResolve: (() => void) | null = null;
    private _authReject: ((e: Error) => void) | null = null;

    static async connect(socketPath: string, secret: Buffer): Promise<SkyWorkerSocket> {
        const instance = new SkyWorkerSocket();
        instance._secret = secret;

        const authDone = new Promise<void>((res, rej) => {
            instance._authResolve = res;
            instance._authReject  = rej;
        });

        instance.socket = await Bun.connect({
            unix: socketPath,
            socket: {
                data(_socket, chunk) {
                    instance.onData(chunk);
                },
                drain(_socket) {
                    instance.onDrain();
                },
                open(socket) {
                    instance.socket = socket;
                },
                close(_socket) {
                    instance.onClose();
                },
                error(_socket, err) {
                    instance.onError(err);
                }
            }
        });

        await authDone;
        return instance;
    }

    /// Called by server.ts to be notified when a DRAIN frame arrives.
    onDrained(cb: () => void): void {
        this.drainedCallback = cb;
    }

    /// Called by server.ts to be notified when the socket closes unexpectedly.
    onSocketClosed(cb: () => void): void {
        this.socketClosedCallback = cb;
    }

    // -------------------------------------------------------------------------
    // Inbound — reading frames from the gateway
    // -------------------------------------------------------------------------

    private onData(chunk: Buffer): void {
        // Grow buffer if needed — amortised, not per-chunk
        const needed = this.readBufLen + chunk.length;
        if (needed > this.readBuf.length) {
            const grown = Buffer.allocUnsafe(Math.max(needed, this.readBuf.length * 2));
            this.readBuf.copy(grown, 0, 0, this.readBufLen);
            this.readBuf = grown;
        }

        chunk.copy(this.readBuf, this.readBufLen);
        this.readBufLen += chunk.length;

        // Process all complete frames in the buffer
        let offset = 0;
        while (this.readBufLen - offset >= HEADER_SIZE) {
            const header = decodeHeader(this.readBuf, offset);
            const frameEnd = offset + HEADER_SIZE + header.payloadLen;
            if (frameEnd > this.readBufLen) break;

            const payload = this.readBuf.subarray(offset + HEADER_SIZE, frameEnd);
            this.handleFrame(header, payload);
            offset = frameEnd;
        }

        // Compact — shift remaining bytes to front
        if (offset > 0) {
            this.readBuf.copy(this.readBuf, 0, offset, this.readBufLen);
            this.readBufLen -= offset;
        }
    }

    private createIterable(requestId: number): AsyncIterable<Uint8Array> {
        const self = this;
        return {
            [Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
                return {
                    async next(): Promise<IteratorResult<Uint8Array>> {
                        return new Promise((resolve, reject) => {
                            const stream = self.activeBodyStreams.get(requestId);
                            if (!stream) {
                                resolve({ done: true, value: undefined });
                                return;
                            }
                            // Drain any chunks that arrived before next() was called.
                            const buffered = stream.chunks.shift();
                            if (buffered !== undefined) {
                                if (stream.done && stream.chunks.length === 0) {
                                    self.activeBodyStreams.delete(requestId);
                                }
                                resolve({ done: false, value: buffered });
                                return;
                            }
                            // Stream is done and nothing is buffered.
                            if (stream.done) {
                                self.activeBodyStreams.delete(requestId);
                                resolve({ done: true, value: undefined });
                                return;
                            }
                            stream.waiters.push({ resolve, reject });
                        });
                    }
                };
            }
        };
    }

    private handleFrame(header: FrameHeader, payload: Buffer): void {
        switch (header.frameType) {

            case FrameType.Invoke: {
                const data = decode(payload) as Record<string, unknown>;
                const isStreaming = data['stream'] as boolean ?? false;

                if (isStreaming) {
                    this.activeBodyStreams.set(header.requestId, {
                        waiters: [],
                        chunks: [],
                        done: false
                    });
                    this.sendCredit(header.requestId, 64 * 1024);
                }

                const inv: SkyInvocation = {
                    requestId: header.requestId,
                    handlerId: data["handler_id"] as string,
                    method: data["method"] as string,
                    path: data["path"] as string,
                    params: (data["params"] ?? {}) as Record<string, string>,
                    query: (data["query"] ?? {}) as Record<string, string>,
                    headers: (data["headers"] ?? {}) as Record<string, string>,
                    body: data["body"] as Uint8Array,
                    streamedBody: data['stream'] ? this.createIterable(header.requestId) : null
                };

                // Deliver to a waiting consumer or enqueue for the next iteration
                const waiter = this.waiters.shift();
                if (waiter) waiter({ ok: true, inv });
                else this.queue.push(inv);
                break;
            }

            case FrameType.InvokeBodyChunk: {
                const stream = this.activeBodyStreams.get(header.requestId);
                if (!stream) {
                    console.warn(`[sky/transport] unexpected body chunk for request ${header.requestId}`);
                    break;
                }

                // Copy payload bytes — payload is a subarray view into readBuf, which
                // gets compacted (overwritten) after this frame is processed. Promises
                // resolve asynchronously (microtask), so the consumer would see corrupt
                // bytes without this copy.
                const chunk = Buffer.from(payload);

                const waiter = stream.waiters.shift();
                if (waiter) {
                    waiter.resolve({ done: false, value: chunk });
                } else {
                    stream.chunks.push(chunk);
                }

                this.sendCredit(header.requestId, payload.byteLength);
                break;
            }

            case FrameType.InvokeEnd: {
                const stream = this.activeBodyStreams.get(header.requestId);
                if (!stream) {
                    console.warn(`[sky/transport] INVOKE_END for unknown stream ${header.requestId}`);
                    break;
                }

                stream.done = true;

                // Invariant: waiters and chunks are mutually exclusive (next() drains
                // chunks before parking a waiter). So if there are waiters, there are
                // no buffered chunks — resolve them with done immediately.
                for (const waiter of stream.waiters) {
                    waiter.resolve({ done: true, value: undefined });
                }
                stream.waiters = [];

                // Only remove from the map now if chunks are fully drained.
                // If chunks remain, next() will clean up after draining them.
                if (stream.chunks.length === 0) {
                    this.activeBodyStreams.delete(header.requestId);
                }
                break;
            }

            case FrameType.InvokeCancel: {
                // Cancel request body stream, if one was in progress.
                const stream = this.activeBodyStreams.get(header.requestId);
                if (stream) {
                    const err = new Error('request cancelled by gateway');
                    for (const waiter of stream.waiters) waiter.reject(err);
                    stream.waiters = [];
                    this.activeBodyStreams.delete(header.requestId);
                }
                // Release any sendChunk() awaiting response credit for this
                // request. Without this the handler stalls forever once the
                // initial 1 MiB pre-credit is exhausted.
                const credit = this.responseCredits.get(header.requestId);
                if (credit) {
                    const waiters = credit.waiters;
                    this.responseCredits.delete(header.requestId);
                    for (const w of waiters) w();
                }
                break;
            }

            case FrameType.ResponseCredit: {
                const { allowedBytes } = decode(payload) as { allowedBytes: number };
                let entry = this.responseCredits.get(header.requestId);
                if (!entry) {
                    entry = { bytes: 0, waiters: [] };
                    this.responseCredits.set(header.requestId, entry);
                }
                entry.bytes += allowedBytes;
                const waiters = entry.waiters;
                entry.waiters = [];
                for (const w of waiters) w();
                break;
            }

            case FrameType.AuthChallenge: {
                const hmac = createHmac('sha256', this._secret!);
                hmac.update(payload);
                const digest = hmac.digest();
                this.writeRaw(FrameType.AuthResponse, 0, digest);
                this._authResolve?.();
                this._authResolve = null;
                this._authReject  = null;
                break;
            }

            case FrameType.Ping: {
                // Auto-reply — gateway uses this for health checks
                this.writeRaw(FrameType.Pong, 0, payload);
                break;
            }

            case FrameType.Drain: {
                // Gateway-initiated graceful shutdown.
                // Stop accepting new invocations. In-flight handlers run to completion.
                this.draining = true;
                this.resolveWaiters("drain");
                this.drainedCallback?.();
                break;
            }

            default:
                console.warn(`[sky/transport] unexpected inbound frame type: ${header.frameType}`);
        }
    }

    // -------------------------------------------------------------------------
    // Shutdown paths
    // -------------------------------------------------------------------------

    private onError(err: Error): void {
        console.error("[sky/transport] socket error:", err.message);
        // onClose fires immediately after — shutdown handled there
    }

    private onClose(): void {
        if (this.draining) {
            // Already shutting down via DRAIN frame — nothing extra needed
            return;
        }

        // Unexpected close — gateway crashed or connection dropped.
        // Unblock all parked waiters immediately so the process can exit.
        this.draining = true;
        this.resolveWaiters("socket_closed");
        this.socketClosedCallback?.();
    }

    private onDrain(): void {
        this.flushWriteQueue();
    }

    /// Resolve all parked waiters with a shutdown result.
    /// Called on both DRAIN frame and unexpected socket close.
    private resolveWaiters(reason: "drain" | "socket_closed"): void {
        for (const waiter of this.waiters) {
            waiter({ ok: false, reason });
        }
        this.waiters = [];
    }

    // -------------------------------------------------------------------------
    // invocations() — public async generator for the dispatcher
    // -------------------------------------------------------------------------

    /// Yields one SkyInvocation per INVOKE frame received from the gateway.
    ///
    /// Exits cleanly when:
    ///   - A DRAIN frame is received (gateway-initiated graceful shutdown)
    ///   - The socket closes unexpectedly (gateway crash / connection drop)
    ///
    /// The dispatcher distinguishes these two cases via the exit reason
    /// to decide whether to wait for in-flight handlers or exit immediately.
    async *invocations(): AsyncGenerator<SkyInvocation, void, undefined> {
        while (!this.draining) {
            const result = await new Promise<InvocationResult>((resolve) => {
                // Re-check draining inside the Promise constructor — a DRAIN frame
                // may have arrived between the while condition and this execution
                if (this.draining) {
                    resolve({ ok: false, reason: "drain" });
                    return;
                }

                const queued = this.queue.shift();
                if (queued) {
                    resolve({ ok: true, inv: queued });
                } else {
                    // Park until an INVOKE frame arrives or a shutdown path fires
                    this.waiters.push(resolve);
                }
            });

            if (!result.ok) {
                if (result.reason === "socket_closed") {
                    console.error("[sky/transport] socket closed unexpectedly — exiting without drain");
                } else {
                    console.log("[sky/transport] drain received — stopping after in-flight complete");
                }
                break;
            }

            yield result.inv;
        }
    }

    // -------------------------------------------------------------------------
    // Outbound — sending response frames to the gateway
    // -------------------------------------------------------------------------

    sendHead(
        requestId: number,
        status: number,
        headers: Record<string, string>,
    ): void {
        this.writeRaw(FrameType.ResponseHead, requestId, encode({ status, headers }));
    }

    /// Send a body chunk for a non-streaming (buffered) response. Skips the
    /// response-credit check — the gateway doesn't send credit for buffered
    /// routes, and kernel backpressure via the writeQueue is sufficient.
    sendChunkDirect(requestId: number, data: Uint8Array): void {
        this.writeRaw(FrameType.ResponseChunk, requestId, data);
    }

    /// Send a body chunk. Awaits both response credit (gateway flow control)
    /// and socket-buffer drain (kernel backpressure) so no bytes are ever
    /// silently dropped. Use only for streaming (AsyncGenerator) responses.
    async sendChunk(requestId: number, data: Uint8Array): Promise<void> {
        await this.awaitResponseCredit(requestId, data.byteLength);
        this.writeRaw(FrameType.ResponseChunk, requestId, data);
        if (this.writeQueue.length > 0) {
            await this.awaitWriteDrain();
        }
    }

    sendEnd(requestId: number): void {
        this.writeRaw(FrameType.ResponseEnd, requestId, new Uint8Array(0));
        this.responseCredits.delete(requestId);
    }

    sendError(
        requestId: number,
        code: string,
        message: string,
        trace?: string,
    ): void {
        this.writeRaw(FrameType.Error, requestId, encode({ code, message, trace }));
        this.responseCredits.delete(requestId);
    }

    sendCredit(requestId: number, bufferSize: number): void {
        this.writeRaw(FrameType.InvokeCredit, requestId, encode({ allowedBytes: bufferSize }));
    }

    // -------------------------------------------------------------------------
    // Backpressure helpers
    // -------------------------------------------------------------------------

    /// Block until at least `bytes` of response credit is available for
    /// `requestId`. The gateway grants credit by sending RESPONSE_CREDIT frames
    /// as it forwards chunks to the HTTP client.
    private async awaitResponseCredit(requestId: number, bytes: number): Promise<void> {
        let entry = this.responseCredits.get(requestId);
        if (!entry) {
            entry = { bytes: 0, waiters: [] };
            this.responseCredits.set(requestId, entry);
        }
        while (entry.bytes < bytes) {
            await new Promise<void>(resolve => entry!.waiters.push(resolve));
            // Re-fetch in case sendEnd cleared the entry while waiting.
            const refreshed = this.responseCredits.get(requestId);
            if (!refreshed) return;
            entry = refreshed;
        }
        entry.bytes -= bytes;
    }

    private async awaitWriteDrain(): Promise<void> {
        if (this.writeQueue.length === 0) return;
        return new Promise<void>(resolve => this.drainWaiters.push(resolve));
    }

    // -------------------------------------------------------------------------
    // Internal write — handles partial writes via writeQueue + onDrain
    // -------------------------------------------------------------------------

    private writeRaw(frameType: number, requestId: number, payload: Uint8Array): void {
        const totalLen = HEADER_SIZE + payload.length;

        // Fast path: queue is empty and frame fits in sendBuf — write directly
        // with zero allocation. socket.write() copies bytes to the kernel
        // synchronously, so sendBuf is safe to reuse immediately on a full write.
        if (this.writeQueue.length === 0 && totalLen <= this.sendBuf.length) {
            this.sendBuf[0] = PROTOCOL_VERSION;
            this.sendBuf[1] = frameType;
            this.sendBuf.writeUInt32BE(requestId, 2);
            this.sendBuf.writeUInt32BE(payload.length, 6);
            if (payload.length > 0) {
                this.sendBuf.set(payload, HEADER_SIZE);
            }
            const written = this.socket.write(this.sendBuf.subarray(0, totalLen));
            if (written < 0) return; // socket closed
            if (written === totalLen) return; // full write — done, no allocation
            // Partial or zero-byte write — copy unsent tail into an owned buffer.
            // Drain event will call flushWriteQueue when kernel buffer has space.
            this.writeQueue.push(Buffer.from(this.sendBuf.subarray(written, totalLen)));
            return;
        }

        // Slow path: queue has frames (must preserve ordering) or frame > 64KB
        const frame = Buffer.allocUnsafe(totalLen);
        frame[0] = PROTOCOL_VERSION;
        frame[1] = frameType;
        frame.writeUInt32BE(requestId, 2);
        frame.writeUInt32BE(payload.length, 6);
        if (payload.length > 0) {
            frame.set(payload, HEADER_SIZE);
        }
        this.writeQueue.push(frame);
        // If nothing else was queued before this push, start the flush.
        // Otherwise we're already mid-flush (waiting for drain event).
        if (this.writeQueue.length === 1) {
            this.flushWriteQueue();
        }
    }

    private flushWriteQueue(): void {
        while (this.writeQueue.length > 0) {
            const frame = this.writeQueue[0]!;
            const written = this.socket.write(frame);
            if (written < 0) return; // socket closed
            if (written === frame.length) {
                this.writeQueue.shift();
                continue;
            }
            if (written > 0) {
                // Partial write — replace the head with the unsent tail.
                this.writeQueue[0] = frame.subarray(written);
            }
            // Either way (0 or partial), wait for drain.
            return;
        }
        // Queue is empty — wake any sendChunk awaiters.
        if (this.drainWaiters.length > 0) {
            const waiters = this.drainWaiters;
            this.drainWaiters = [];
            for (const w of waiters) w();
        }
    }

}