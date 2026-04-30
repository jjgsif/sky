/**
 * Sky Demo Application
 *
 * A small but realistic demo exercising the full E2 feature set:
 *
 *   - UserService: CRUD with Zod validation, path params, query params
 *   - HealthService: Singleton health check
 *   - AdminService: Route group with /api/admin prefix, header extraction
 *
 * Build:  sky build
 * Run:    SKY_WORKER_ID=1 bun run index.ts
 * Test:   bun run test.ts
 */

import { Container } from "@blue.ts/di";
import { 
  ServiceRegistry,
  HandlerDispatcher,
  startServer, 
  logger
} from "sky/runtime";

// Application services — importing triggers decorators
import { UserService } from "./services/user";
import { HealthService } from "./services/health";
import { AdminService } from "./services/admin";

// Generated Connect service types from compiled protos
import { UserService as UserServiceProto } from "./src/gen/user_service_pb";
import { HealthService as HealthServiceProto } from "./src/gen/health_service_pb";
import { AdminService as AdminServiceProto } from "./src/gen/admin_service_pb";

async function main() {
  const log = logger.child({ component: "demo" });

  // 1. DI container
  const container = new Container();

  // 2. Register services
  const registry = new ServiceRegistry(container);
  registry.registerAll([UserService, HealthService, AdminService]);

  log.info({ services: registry.serviceNames() }, "services registered");

  // 3. Dispatcher
  const dispatcher = new HandlerDispatcher(container, registry);
  dispatcher.registerServiceType("UserService", UserServiceProto);
  dispatcher.registerServiceType("HealthService", HealthServiceProto);
  dispatcher.registerServiceType("AdminService", AdminServiceProto);

  // 4. Start server
  const server = await startServer({
    workerVersion: "0.1.0",
    logger,
    gracePeriodDefaultMs: 3000,
    dispatcher,
  });

  log.info(
    {
      workerId: server.workerId,
      socketPath: server.socketPath,
      services: registry.serviceNames(),
    },
    "demo application ready"
  );

  // Graceful shutdown
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
