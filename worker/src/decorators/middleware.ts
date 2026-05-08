import type { MiddlewareOptions, SkyClassMetadata } from "./types";

// Two call signatures:
//   @Middleware(SomeClass)          — apply that middleware to all handlers on this service
//   @Middleware({ global, order })  — declare this class as a middleware
const Middleware = (arg?: Function | MiddlewareOptions) => (_value: Function, context: ClassDecoratorContext) => {
    const meta = context.metadata as SkyClassMetadata;
    if (typeof arg === "function") {
        if (!meta.applyMiddleware) meta.applyMiddleware = [];
        meta.applyMiddleware.push(arg);
    } else {
        const options = arg as MiddlewareOptions | undefined;
        meta.middleware = {
            global: options?.global ?? false,
            order: options?.order ?? 0,
            kind: "user",
        };
    }
};

export { Middleware };
