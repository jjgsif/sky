import type { ConnectRouter } from "@connectrpc/connect";
import { WorkerControl } from "@gen/worker_control_pb";
import type { Logger } from "../logger";
import { HealthStatus } from "@gen/worker_control_pb";

/**
 * Mutable worker state that the WorkerControl service reports and manipulates.
 *
 * Held in a module-level object rather than passed around explicitly because
 * the health status is genuinely global to the worker — there's one worker
 * per process, and its status is observable from any handler.
 */
interface WorkerState {
    status: HealthStatus;
    version: string;
    shutdownHook?: () => void;
}

export function createWorkerState(version: string): WorkerState {
    return {
        status: HealthStatus.READY,
        version,
    };
}

export function registerWorkerControl(
    router: ConnectRouter,
    state: WorkerState,
    logger: Logger,
): void {
    router.service(WorkerControl, {
        async health(_req, _context) {
            return {
                status: state.status,
                workerVersion: state.version,
                details: {},
            };
        },

        async shutdown(req, context) {
            const requestId = context.requestHeader.get("x-request-id") ?? "unknown";
            const reqLogger = logger.child({ requestId, rpc: "WorkerControl.Shutdown" });

            reqLogger.info(
                { gracePeriodSeconds: req.gracePeriodSeconds },
                "shutdown requested",
            );

            // Transition to draining so subsequent health checks see the change.
            state.status = HealthStatus.DRAINING;

            // Invoke the shutdown hook on the next tick so the response to
            // this RPC makes it back to the caller before the process exits.
            if (state.shutdownHook) {
                const hook = state.shutdownHook;
                queueMicrotask(() => hook());
            }

            return {
                accepted: true,
            };
        },
    });
}