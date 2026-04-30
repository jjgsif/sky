import { Service, Handler } from "@decorators";

@Service({ lifetime: "singleton" })
export class HealthService {

    @Handler({
        method: "GET",
        path: "/health",
    })
    async check(): Promise<{ status: string }> {
        return { status: "ok" };
    }
}