import { connectNodeAdapter } from "@connectrpc/connect-node";
import type { ConnectRouter } from "@connectrpc/connect";
import * as http from "node:http2";
import * as fs from "node:fs";
import type { Logger } from "@/logger";
import { registerHelloService } from "@/services/hello";
import {
    registerWorkerControl,
    createWorkerState,
} from "@/services/worker_control";

export interface ServerOptions {
    socketPath: string;
    workerVersion: string;
    logger: Logger;
    gracePeriodDefaultMs: number;
}

export interface RunningServer {
    close: () => Promise<void>;
}

/**
 * Start the Sky worker's Connect server listening on a Unix domain socket.
 *
 * This function is responsible for:
 *   - Registering all Connect services on a single router.
 *   - Cleaning up any stale socket file from a previous unclean shutdown.
 *   - Binding the HTTP server to the socket.
 *   - Wiring the WorkerControl shutdown hook to actually stop the server.
 *   - Returning a handle the caller can use to close the server on signals.
 */
export async function startServer(opts: ServerOptions): Promise<RunningServer> {
    const { socketPath, workerVersion, logger, gracePeriodDefaultMs } = opts;

    // Remove any stale socket file from a previous unclean shutdown.
    // Safe because Phase 1 runs only one worker per socket path.
    if (fs.existsSync(socketPath)) {
        logger.warn({ socketPath }, "removing stale socket file");
        fs.unlinkSync(socketPath);
    }

    const workerState = createWorkerState(workerVersion);

    const handler = connectNodeAdapter({
        routes(router: ConnectRouter) {
            registerHelloService(router, logger);
            registerWorkerControl(router, workerState, logger);
        },
    });

    const server = http.createServer(handler);

    // Install the shutdown hook that WorkerControl.Shutdown will invoke.
    // Flow: shutdown RPC arrives -> handler sets status to DRAINING ->
    // hook is scheduled via queueMicrotask -> hook runs close() ->
    // process exits.
    workerState.shutdownHook = () => {
        logger.info({ gracePeriodDefaultMs }, "shutdown hook invoked; closing server");
        void closeAndExit(server, socketPath, logger, gracePeriodDefaultMs);
    };

    // Bind to the socket and wait for it to be ready.
    await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(socketPath, () => {
            server.removeListener("error", reject);
            resolve();
        });
    });

    logger.info({ socketPath }, "worker listening");

    return {
        close: async () => {
            await closeAndExit(server, socketPath, logger, gracePeriodDefaultMs);
        },
    };
}

async function closeAndExit(
    server: http.Http2Server,
    socketPath: string,
    logger: Logger,
    gracePeriodMs: number,
): Promise<void> {
    // Wait for the grace period to let in-flight requests complete.
    // Phase 1 uses a simple sleep; E1-S8 will track in-flight count precisely.
    await new Promise((resolve) => setTimeout(resolve, gracePeriodMs));

    await new Promise<void>((resolve) => {
        server.close(() => {
            if (fs.existsSync(socketPath)) {
                try {
                    fs.unlinkSync(socketPath);
                } catch (err) {
                    logger.warn({ err }, "failed to remove socket file on exit");
                }
            }
            logger.info("server closed; exiting process");
            resolve();
        });
    });

    // Exit cleanly after server close completes.
    process.exit(0);
}