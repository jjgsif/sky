import type { HandlerDefinition, HandlerOptions, HttpMethod } from "./types";

const Handler = (options: HandlerOptions) => {
    return (_value: Function, context: ClassMethodDecoratorContext) => {
        if (!context.metadata.handlers) {
            context.metadata.handlers = new Map<string, HandlerDefinition>();
        }

        const def: HandlerDefinition = {
            status: options.status ?? getDefaultStatus(options.method),
            extract: options.extract ?? [],
            method: options.method,
            path: options.path,
            validate: options.validate ?? true,
            streaming: options.streaming ?? false,
        };
        if (options.middleware?.length) def.middleware = options.middleware;
        (context.metadata.handlers as Map<string, HandlerDefinition>).set(context.name.toString(), def);
    };
};

function getDefaultStatus(method: HttpMethod): number {
    switch (method) {
        case "POST": return 201;
        case "DELETE": return 204;
        default: return 200;
    }
}

export { Handler };