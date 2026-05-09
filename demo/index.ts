/**
 * Sky Demo Application
 *
 * A small but realistic demo exercising the full E2 feature set:
 *
 *   - UserService: CRUD with Zod validation, path params, query params
 *   - HealthService: Singleton health check
 *   - AdminService: Route group with /api/admin prefix, header extraction
 *
 * Run:    SKY_WORKER_ID=1 bun run index.ts
 * Test:   bun run test.ts
 */

import { startServer, logger } from "sky/runtime";

// Application services — importing triggers decorators
import { UserService } from "./services/user";
import { HealthService } from "./services/health";
import { AdminService } from "./services/admin";
import { HelloService } from "./services/hello";
import { StreamService } from "./services/stream";

async function main() {
  const log = logger.child({ component: "demo" });

  const server = await startServer({
    workerVersion: "0.1.0",
    logger,
    gracePeriodDefaultMs: 3000,
    services: [UserService, HealthService, AdminService, HelloService, StreamService],
  });

  log.info(
    {
      workerId: server.workerId,
      socketPath: server.socketPath,
    },
    "demo application ready"
  );

  const shutdown = async () => {
    log.info("shutting down");
    await server.close();
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

main().catch((err) => {
  console.error("fatal:", err);
  process.exit(1);
});
