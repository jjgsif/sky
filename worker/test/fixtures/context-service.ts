import { Service, Handler, Context, Param, type ExtractContext } from "@sky/decorators";
import type { RequestContext } from "@sky/runtime/context";

interface CreateItemDto {
    name: string;
    price: number;
}

const ctxOnlyExtract = {
    ctx: Context(),
} as const;

const ctxBodyExtract = {
    ctx: Context<CreateItemDto>(),
} as const;

const mixedExtract = {
    id: Param("id"),
    ctx: Context(),
} as const;

@Service({ lifetime: "scoped" })
export class ContextService {
    @Handler({ method: "GET", path: "/me", extract: ctxOnlyExtract })
    async getMe(_input: ExtractContext<typeof ctxOnlyExtract>): Promise<{ sub: string | null }> {
        return { sub: (_input.ctx.claims?.["sub"] as string) ?? null };
    }

    @Handler({ method: "POST", path: "/items", status: 201, extract: ctxBodyExtract })
    async createItem(_input: ExtractContext<typeof ctxBodyExtract>): Promise<{ name: string }> {
        return { name: _input.ctx.body.name };
    }

    @Handler({ method: "GET", path: "/items/:id", extract: mixedExtract })
    async getItem(_input: ExtractContext<typeof mixedExtract>): Promise<{ id: string }> {
        return { id: _input.id };
    }
}
