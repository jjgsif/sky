import { Service, Middleware } from "@sky/decorators";

@Service({ lifetime: "singleton", dependencies: [] })
@Middleware({ global: true, order: 1 })
export class AuthMiddleware {
    async handle(ctx: any, next: () => Promise<void>): Promise<void> {
        await next();
    }
}