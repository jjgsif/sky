// packages/transport/src/socket.ts
//
// Client-side implementation of the Sky framing protocol.
// Internal to the framework — developers never touch this directly.

import { encode, decode } from "@msgpack/msgpack";
import {
    FrameType, HEADER_SIZE, PROTOCOL_VERSION,
    type FrameHeader, type InvocationResult,
    type InvocationWaiter, type SkyInvocation
} from "./types";
import { decodeHeader, encodeHeader } from "./util";

export class SkyWorkerSocket {
    private socket!: ReturnType<typeof Bun.connect> extends Promise<infer S> ? S : never;
    private readBuf: Buffer = Buffer.allocUnsafe(64 * 1024);
    private readBufLen = 0;
    private draining: boolean = false;

    // Decoded invocations not yet consumed by the dispatcher
    private queue: SkyInvocation[] = [];

    // Parked Promise resolve callbacks waiting for the next invocation
    private waiters: InvocationWaiter[] = [];

    // External lifecycle hooks — set by server.ts
    private drainedCallback: (() => void) | null = null;
    private socketClosedCallback: (() => void) | null = null;

    static async connect(socketPath: string): Promise<SkyWorkerSocket> {
        const instance = new SkyWorkerSocket();

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
        })

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

    private handleFrame(header: FrameHeader, payload: Buffer): void {
        switch (header.frameType) {

            case FrameType.Invoke: {
                const data = decode(payload) as Record<string, unknown>;
                const inv: SkyInvocation = {
                    requestId: header.requestId,
                    handlerId: data["handler_id"] as string,
                    method: data["method"] as string,
                    path: data["path"] as string,
                    params: (data["params"] ?? {}) as Record<string, string>,
                    query: (data["query"] ?? {}) as Record<string, string>,
                    headers: (data["headers"] ?? {}) as Record<string, string>,
                    body: data["body"] as Uint8Array,
                };

                // Deliver to a waiting consumer or enqueue for the next iteration
                const waiter = this.waiters.shift();
                if (waiter) waiter({ ok: true, inv });
                else this.queue.push(inv);
                break;
            }

            case FrameType.Ping: {
                // Auto-reply — gateway uses this for health checks
                this.writeRaw(FrameType.Pong, 0, Buffer.from(payload));
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

    private onDrain() { }

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
        const payload = Buffer.from(encode({ status, headers }));
        this.writeRaw(FrameType.ResponseHead, requestId, payload);
    }

    /// Send a body chunk. Raw bytes — no MessagePack envelope.
    sendChunk(requestId: number, data: Uint8Array): void {
        this.writeRaw(FrameType.ResponseChunk, requestId, Buffer.from(data));
    }

    sendEnd(requestId: number): void {
        this.writeRaw(FrameType.ResponseEnd, requestId, Buffer.alloc(0));
    }

    sendError(
        requestId: number,
        code: string,
        message: string,
        trace?: string,
    ): void {
        const payload = Buffer.from(encode({ code, message, trace }));
        this.writeRaw(FrameType.Error, requestId, payload);
    }

    // -------------------------------------------------------------------------
    // Internal write
    // -------------------------------------------------------------------------

    private static readonly sendBuf = Buffer.allocUnsafe(64 * 1024);

    private writeRaw(frameType: number, requestId: number, payload: Buffer): void {
        const totalLen = HEADER_SIZE + payload.length;

        // For small frames use the pre-allocated send buffer — single atomic write
        if (totalLen <= SkyWorkerSocket.sendBuf.length) {
            SkyWorkerSocket.sendBuf[0] = PROTOCOL_VERSION;
            SkyWorkerSocket.sendBuf[1] = frameType;
            SkyWorkerSocket.sendBuf.writeUInt32BE(requestId, 2);
            SkyWorkerSocket.sendBuf.writeUInt32BE(payload.length, 6);
            if (payload.length > 0) {
                payload.copy(SkyWorkerSocket.sendBuf, HEADER_SIZE);
            }
            this.socket.write(SkyWorkerSocket.sendBuf.subarray(0, totalLen));
        } else {
            // Large payload — allocate once, write once
            const frame = Buffer.allocUnsafe(totalLen);
            frame[0] = PROTOCOL_VERSION;
            frame[1] = frameType;
            frame.writeUInt32BE(requestId, 2);
            frame.writeUInt32BE(payload.length, 6);
            payload.copy(frame, HEADER_SIZE);
            this.socket.write(frame);
        }
    }
}