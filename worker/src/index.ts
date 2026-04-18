import { logger } from "@/logger";
import { startServer } from "@/server";

/**
 * Sky worker entry point.
 *
 * Phase 1: starts a single Connect server on a Unix domain socket,
 * serving HelloService and WorkerControl. Configuration is via
 * environment variables; no config file in Phase 1.
 */

async function main(): Promise<void> {
    const socketPath = process.env.SKY_WORKER_SOCKET;
    if (!socketPath) {
        logger.error("SKY_WORKER_SOCKET environment variable is required");
        process.exit(1);
    }

    const workerVersion = process.env.SKY_WORKER_VERSION ?? "dev-0.1.0";
    const gracePeriodDefaultMs = parseGraceMs(
        process.env.SKY_SHUTDOWN_GRACE_MS,
        5000,
    );

    logger.info(
        { socketPath, workerVersion, gracePeriodDefaultMs },
        "sky worker starting",
    );

    const server = await startServer({
        socketPath,
        workerVersion,
        logger,
        gracePeriodDefaultMs,
    });

    // Handle external signals so direct `kill` from the shell also
    // triggers graceful shutdown. The gateway uses the Shutdown RPC
    // in production; signals are the manual fallback.
    const handleSignal = (signal: string) => {
        logger.info({ signal }, "received signal; shutting down");
        void server.close();
    };

    process.on("SIGTERM", () => handleSignal("SIGTERM"));
    process.on("SIGINT", () => handleSignal("SIGINT"));
}

function parseGraceMs(value: string | undefined, defaultMs: number): number {
    if (!value) return defaultMs;
    const parsed = parseInt(value, 10);
    if (Number.isNaN(parsed) || parsed < 0) return defaultMs;
    return parsed;
}

main().catch((err) => {
    logger.error({ err }, "fatal error during startup");
    process.exit(1);
});