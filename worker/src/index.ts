/**
 * Worker Entry Point
 *
 * Bootstraps the Sky worker with HelloService.
 *
 * Run with: SKY_WORKER_ID=1 bun run src/index.ts
 */

import { startServer } from "./runtime/server";
import { logger } from "./runtime/logger";

import { HelloService } from "./runtime/services/hello";
import { StreamService } from "./runtime/services/stream";

async function main() {
  const server = await startServer({
    workerVersion: "0.1.0",
    logger,
    gracePeriodDefaultMs: 3000,
    services: [HelloService, StreamService],
  });

  logger.info(
    {
      workerId: server.workerId,
      socketPath: server.socketPath,
    },
    "worker ready"
  );

  const shutdown = async () => {
    logger.info("received shutdown signal");
    await server.close();
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

main().catch((err) => {
  console.error("fatal: worker failed to start", err);
  process.exit(1);
});
