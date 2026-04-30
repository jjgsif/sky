import { connectNodeAdapter } from "@connectrpc/connect-node";
import type { ConnectRouter } from "@connectrpc/connect";
import http from "node:http2";
import * as fs from "node:fs";
import type { Logger } from "./logger";
import {
    registerWorkerControl,
    createWorkerState,
} from "@runtime/services/worker_control";
import type { HandlerDispatcher } from "./dispatcher";

// ── Constants ───────────────────────────────────────────

const SOCKET_DIR = "/tmp/sky/workers";
const SOCKET_PREFIX = "sky-worker";

// ── Types ───────────────────────────────────────────────

export interface ServerOptions {
    workerVersion: string;
    logger: Logger;
    gracePeriodDefaultMs: number;
    dispatcher: HandlerDispatcher;
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

    // Ensure the socket directory exists.
    if (!fs.existsSync(SOCKET_DIR)) {
        fs.mkdirSync(SOCKET_DIR, { recursive: true });
    }

    const socketPath = `${SOCKET_DIR}/${SOCKET_PREFIX}-${workerId}.sock`;

    return { socketPath, workerId };
}

// ── Server ──────────────────────────────────────────────

/**
 * Start the Sky worker's Connect server listening on a Unix domain socket.
 *
 * Each worker instance gets a unique socket path derived from its
 * SKY_WORKER_ID. The server registers all Connect services on a
 * single router:
 *
 *   - WorkerControl: health checks, shutdown, status reporting
 *   - Per-service handlers: dynamically registered from the dispatcher
 *
 * The dispatcher handles all application-level RPC routing internally,
 * resolving the target service and handler via the DI container.
 */
export async function startServer(opts: ServerOptions): Promise<RunningServer> {
    const { workerVersion, logger, gracePeriodDefaultMs, dispatcher } = opts;

    const { socketPath, workerId } = resolveSocketPath(logger);

    // Clean up any stale socket from a previous unclean shutdown
    // of this specific worker ID.
    if (fs.existsSync(socketPath)) {
        logger.warn({ socketPath, workerId }, "removing stale socket file");
        fs.unlinkSync(socketPath);
    }

    const workerState = createWorkerState(workerVersion);

    const handler = connectNodeAdapter({
        routes(router: ConnectRouter) {
            registerWorkerControl(router, workerState, logger);
            dispatcher.registerServices(router);
        },
    });

    const server = http.createServer(
        handler
    );

    // Install the shutdown hook that WorkerControl.Shutdown will invoke.
    workerState.shutdownHook = () => {
        logger.info(
            { gracePeriodDefaultMs, workerId },
            "shutdown hook invoked; closing server"
        );
        void closeAndExit(server, socketPath, workerId, logger, gracePeriodDefaultMs);
    };

    // Bind to the socket and wait for it to be ready.
    await new Promise<void>((resolve, reject) => {
        server.once("error", (err) => {
            logger.error({ err, socketPath, workerId }, "server bind failed");
            reject(err);
        });
        server.listen(socketPath, () => {
            server.removeListener("error", reject);
            resolve();
        });
    });

    logger.info({ socketPath, workerId }, "worker listening");

    return {
        socketPath,
        workerId,
        close: async () => {
            await closeAndExit(server, socketPath, workerId, logger, gracePeriodDefaultMs);
        },
    };
}

// ── Shutdown ────────────────────────────────────────────

async function closeAndExit(
    server: http.Http2Server,
    socketPath: string,
    workerId: string,
    logger: Logger,
    gracePeriodMs: number,
): Promise<void> {
    // Wait for the grace period to let in-flight requests complete.
    await new Promise((resolve) => setTimeout(resolve, gracePeriodMs));

    await new Promise<void>((resolve) => {
        server.close(() => {
            if (fs.existsSync(socketPath)) {
                try {
                    fs.unlinkSync(socketPath);
                } catch (err) {
                    logger.warn({ err, workerId }, "failed to remove socket file on exit");
                }
            }
            logger.info({ workerId }, "server closed; exiting process");
            resolve();
        });
    });

    process.exit(0);
}
