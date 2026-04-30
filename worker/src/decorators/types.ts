interface ServiceOptions {
    lifetime?: "singleton" | "scoped" | "transient";
    name?: string;
    dependencies?: (string | Function | symbol)[];
}

interface ServiceRegistration {
    lifetime: "singleton" | "scoped" | "transient";
    name?: string;
    dependencies: (string | Function | symbol)[];
    handlers: Map<string, HandlerDefinition>;
    middleware?: MiddlewareDefinition,
    group?: GroupDefinition
}

interface BodyDescriptor { source: "body" }
interface HeaderDescriptor { source: "header"; name: string }
interface QueryDescriptor { source: "query"; name: string }
interface ParamDescriptor { source: "param"; name: string }

type ExtractDescriptor = BodyDescriptor | HeaderDescriptor | QueryDescriptor | ParamDescriptor;

type HttpMethod = "GET" | "POST" | "PATCH" | "PUT" | "DELETE";

interface HandlerDefinition {
    method: HttpMethod;
    path: string;
    status: number;
    extract: ExtractDescriptor[];
    validate: boolean;
}

interface HandlerOptions {
    path: string;
    method: HttpMethod;
    status?: number;
    extract?: ExtractDescriptor[];
    validate?: boolean;
}

interface SkyClassMetadata {
    handlers?: Map<string, HandlerDefinition>;
    group?: GroupDefinition;
    middleware?: MiddlewareDefinition;
}

interface GroupDefinition {
    prefix: string;
    middleware: (Function)[]
}

interface GroupOptions {
    prefix: string;
    middleware?: (Function)[]
}

interface MiddlewareOptions {
    global?: boolean;
    order?: number;
}

interface MiddlewareDefinition {
    global: boolean;
    order: number;
    kind: "user" | "native";
}

export type {
    ServiceOptions,
    ServiceRegistration,
    BodyDescriptor,
    HeaderDescriptor,
    QueryDescriptor,
    ParamDescriptor,
    ExtractDescriptor,
    HttpMethod,
    HandlerDefinition,
    HandlerOptions,
    GroupDefinition,
    GroupOptions,
    MiddlewareDefinition,
    MiddlewareOptions,
    SkyClassMetadata
};