import type { ServiceOptions, ServiceRegistration, SkyClassMetadata } from "./types";

const ServiceMap = new Map<Function, ServiceRegistration>();

const Service = (options?: ServiceOptions) => {
    return (value: Function, context: ClassDecoratorContext) => {
        const metadata = context.metadata as SkyClassMetadata;

        ServiceMap.set(value, {
            lifetime: options?.lifetime ?? "scoped",
            name: options?.name,
            dependencies: options?.dependencies ?? [],
            handlers: metadata.handlers ?? new Map(),
            group: metadata.group,
            middleware: metadata.middleware
        });
    };
};

export {
    Service,
    ServiceMap
};