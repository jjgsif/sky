/**
 * Worker Entry Point
 *
 * Bootstraps the Sky worker with HelloService to verify the
 * full pipeline: decorators → DI → dispatcher → Connect → gRPC.
 *
 * Run with: SKY_WORKER_ID=1 bun run index.ts
 * Test with: bun run smoke-test.ts
 */

import { Container } from "@blue.ts/di";
import { ServiceRegistry } from "./runtime/service-registry";
import { HandlerDispatcher } from "./runtime/dispatcher";
import { startServer } from "./runtime/server";
import { logger } from "./runtime/logger";

// Application service — importing triggers @Service decorator,
// which populates ServiceMap.
import { HelloService } from "./runtime/services/hello";

// Generated Connect service type from compiled proto.
// After running: buf generate (or protoc) on hello_service.proto
import { HelloService as HelloServiceProto } from "@gen/hello_pb";

async function main() {
  const log = logger.child({ component: "bootstrap" });

  // 1. Create root DI container
  const container = new Container();

  // 2. Register application services with DI
  const registry = new ServiceRegistry(container);
  registry.registerAll([HelloService]);

  log.info({ services: registry.serviceNames() }, "services registered");

  // 3. Create dispatcher and register Connect service types
  const dispatcher = new HandlerDispatcher(container, registry);
  dispatcher.registerServiceType("HelloService", HelloServiceProto);

  // 4. Start server on Unix domain socket
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
    },
    "worker ready"
  );

  // Graceful shutdown on signals
  const shutdown = async () => {
    log.info("received shutdown signal");
    await server.close();
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

main().catch((err) => {
  console.error("fatal: worker failed to start", err);
  process.exit(1);
});