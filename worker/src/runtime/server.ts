// packages/worker/src/server.ts

import * as fs from "node:fs";
import type { Logger } from "./logger";
import { Container } from "@blue.ts/di";
import { SkyWorkerSocket } from "../transport";
import { ServiceRegistry } from "./service-registry";
import { createDispatcher } from "./dispatcher";
import type { SkyMiddleware } from "./middleware/types";

// ── Constants ───────────────────────────────────────────

const SOCKET_DIR = "/tmp/sky/workers";
const SOCKET_PREFIX = "sky-worker";

// ── Types ───────────────────────────────────────────────

export interface ServerOptions {
    workerVersion: string;
    logger: Logger;
    gracePeriodDefaultMs: number;
    /**
     * All @Service-decorated classes to register.
     * Passed in from the application entrypoint — the server
     * does not scan for them itself.
     */
    services: (new (...args: any[]) => any)[];
    /**
     * User-defined middleware classes to make available to the dispatcher.
     * Each class is instantiated once (singleton per worker process) and
     * resolved by class name when the manifest declares middleware on a service or handler.
     */
    middleware?: (new (...args: any[]) => SkyMiddleware)[];
}

export interface RunningServer {
    /** The full path to the Unix domain socket this worker is listening on. */
    socketPath: string;

    /** The worker ID assigned by the supervisor. */
    workerId: string;

    /** Gracefully shut down the server. */
    close: () => Promise<void>;
}

// ── Socket path ─────────────────────────────────────────

/**
 * Build the socket path for this worker instance.
 *
 * Convention: /tmp/sky/workers/sky-worker-{workerId}.sock
 *
 * The worker ID is assigned by the Rust supervisor via the
 * SKY_WORKER_ID environment variable. Each worker gets its own
 * socket so the supervisor can address them individually.
 */
function resolveSocketPath(logger: Logger): { socketPath: string; workerId: string } {
    const workerId = process.env.SKY_WORKER_ID;

    if (!workerId) {
        logger.error("SKY_WORKER_ID environment variable is not set");
        process.exit(1);
    }

    if (!fs.existsSync(SOCKET_DIR)) {
        fs.mkdirSync(SOCKET_DIR, { recursive: true });
    }

    const socketPath = `${SOCKET_DIR}/${SOCKET_PREFIX}-${workerId}.sock`;

    return { socketPath, workerId };
}

// ── Server ──────────────────────────────────────────────

/**
 * Start the Sky worker server listening on a Unix domain socket.
 *
 * Architecture:
 *   - Rust gateway creates and owns the socket file
 *   - This worker connects to it via SkyWorkerSocket
 *   - The Sky framing protocol carries INVOKE frames inbound
 *     and RESPONSE_HEAD / RESPONSE_CHUNK / RESPONSE_END frames outbound
 *   - ServiceRegistry reads from ServiceMap populated by @sky/decorators
 *     at decoration time — no generated registry file needed
 *   - The dispatcher resolves services via the DI container per request
 *
 * Usage from application entrypoint:
 *
 *   import { HelloService } from "./services/HelloService";
 *
 *   await startServer({
 *     workerVersion:        "1.0.0",
 *     logger,
 *     gracePeriodDefaultMs: 5000,
 *     services:             [HelloService, UserService],
 *   });
 */
export async function startServer(opts: ServerOptions): Promise<RunningServer> {
    const { workerVersion, logger, gracePeriodDefaultMs, services, middleware } = opts;

    const { socketPath, workerId } = resolveSocketPath(logger);

    // ── DI container and service registry ────────────────

    const container = new Container();
    const registry = new ServiceRegistry(container);

    // Register all @Service-decorated classes.
    // ServiceRegistry reads their ServiceRegistration from ServiceMap
    // which was populated when the decorators ran at import time.
    registry.registerAll(services);

    // ── Middleware registry ───────────────────────────────

    const middlewareMap = new Map<string, SkyMiddleware>();
    for (const mCls of (middleware ?? [])) {
        middlewareMap.set(mCls.name, new mCls());
    }

    // ── Dispatcher ───────────────────────────────────────

    const dispatcher = await createDispatcher(registry, middlewareMap);

    // ── Socket ───────────────────────────────────────────

    // Connect to the gateway-owned socket.
    // The gateway creates and binds the socket before spawning us.
    const skySocket = await SkyWorkerSocket.connect(socketPath);

    logger.info({ socketPath, workerId, workerVersion }, "worker connected to gateway socket");

    // Start the dispatcher — drives the invocations() generator loop
    const stopDispatcher = dispatcher.start(skySocket);

    // ── Shutdown ─────────────────────────────────────────

    let closing = false;

    async function close(): Promise<void> {
        if (closing) return;
        closing = true;

        logger.info({ workerId, gracePeriodDefaultMs }, "shutdown initiated");

        // Stop accepting new invocations
        stopDispatcher();

        // Wait for the grace period to allow in-flight requests to complete
        await new Promise((resolve) => setTimeout(resolve, gracePeriodDefaultMs));

        logger.info({ workerId }, "grace period elapsed — exiting");
        process.exit(0);
    }

    // DRAIN frame — gateway-initiated graceful shutdown
    skySocket.onDrained(() => {
        logger.info({ workerId }, "DRAIN received from gateway — finishing in-flight requests");
        void close();
    });

    // Unexpected socket close — gateway crashed or connection dropped
    skySocket.onSocketClosed(() => {
        logger.error({ workerId }, "gateway socket closed unexpectedly — exiting");
        process.exit(1);
    });

    // OS-level signals
    process.once("SIGTERM", () => {
        logger.info({ workerId }, "SIGTERM received");
        void close();
    });

    process.once("SIGINT", () => {
        logger.info({ workerId }, "SIGINT received");
        void close();
    });

    return {
        socketPath,
        workerId,
        close,
    };
}