import { Service, Handler } from "sky/decorators";

@Service({ lifetime: "singleton" })
class HealthService {
  private readonly startedAt = new Date().toISOString();

  @Handler({
    method: "GET",
    path: "/health",
  })
  async check() {
    return {
      status: "healthy",
      uptime: this.uptime(),
      startedAt: this.startedAt,
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
