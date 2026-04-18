import type { ConnectRouter } from "@connectrpc/connect";
import {HelloService} from "@gen/hello_pb";
import type {Logger} from "@/logger";

export function registerHelloService(
    router: ConnectRouter,
    logger: Logger
): void {
    router.service(HelloService, {
        async greet(req, context) {
            const requestId = context.requestHeader.get("x-request-id") ?? "unknown";
            const reqLogger = logger.child({requestId, rpc: "HelloService.Greet"});

            reqLogger.info({name: req.name}, "Handling Greet Request");

            return {
                message: `Hello ${req.name}`
            }
        }
    })
}