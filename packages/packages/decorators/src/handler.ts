import { HandlerDefinition, HandlerOptions, HttpMethod } from "./types";

const Handler = (options: HandlerOptions) => {
    return (_value: Function, context: ClassMethodDecoratorContext) => {
        if (!context.metadata.handlers) {
            context.metadata.handlers = new Map<string, HandlerDefinition>();
        }

        (context.metadata.handlers as Map<string, HandlerDefinition>).set(
            context.name.toString(),
            {
                status: options.status ?? getDefaultStatus(options.method),
                extract: options.extract ?? [],
                method: options.method,
                path: options.path,
                validate: options.validate ?? true
            }
        );
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