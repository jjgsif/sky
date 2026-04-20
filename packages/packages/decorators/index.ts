// index.ts
export { Service, ServiceMap } from "./src/service";
export { Handler } from "./src/handler";
export { Middleware } from "./src/middleware";
export { Group } from "./src/group";
export { Body, Header, Query, Param } from "./src/descriptor";

export type {
    ServiceRegistration,
    ServiceOptions,
    HandlerDefinition,
    HandlerOptions,
    MiddlewareDefinition,
    MiddlewareOptions,
    GroupDefinition,
    GroupOptions,
    ExtractDescriptor,
    BodyDescriptor,
    HeaderDescriptor,
    QueryDescriptor,
    ParamDescriptor,
    HttpMethod,
    SkyClassMetadata,
} from "./src/types";