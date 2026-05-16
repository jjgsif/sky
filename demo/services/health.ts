import {Service, Handler, Context, type ExtractContext} from "sky-framework/decorators";
import {rateLimit} from "sky-framework/runtime";

const requestExtract = {
  context: Context()
} as const;

@Service({ lifetime: "singleton" })
class HealthService {
  private readonly startedAt = new Date().toISOString();

  @Handler({
    method: "GET",
    path: "/health",
    middleware: [rateLimit({bucket: "api", identifier: "ip", perMinute: 2e10})],
    extract: requestExtract
  })
  async check({context}: ExtractContext<typeof requestExtract>) {
    return {
      status: "healthy",
      uptime: this.uptime(),
      startedAt: this.startedAt,
      workerId: context.headers["x-sky-worker-id"],
      requestId: context.requestId,
    };
  }

  private uptime(): string {
    const started = new Date(this.startedAt).getTime();
    const now = Date.now();
    const seconds = Math.floor((now - started) / 1000);

    if (seconds < 60) return `${seconds}s`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
    return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
  }
}

export { HealthService };
