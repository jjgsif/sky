import type { MiddlewareOptions, SkyClassMetadata } from "./types"

const Middleware = (options?: MiddlewareOptions) => (_value: Function, context: ClassDecoratorContext) => {
    (context.metadata as SkyClassMetadata).middleware = {
        global: options?.global ?? false,
        order: options?.order ?? 0,
        kind: "user"
    };
};

export { Middleware };