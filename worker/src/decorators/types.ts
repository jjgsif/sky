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
    middleware?: MiddlewareDefinition;
    applyMiddleware?: Function[];
    group?: GroupDefinition;
}

// `__t` is a phantom type used purely for inference — not present at runtime
// (the factory casts `null` into it). The function type `(x: T) => T` makes T
// invariant, so `BodyDescriptor<X>` and `BodyDescriptor<Y>` are only assignable
// when X = Y. That precision is what lets `ExtractValue<BodyDescriptor<infer T>>`
// recover the right T inside `ExtractContext`.
interface BodyDescriptor<T = unknown> {
    source: "body";
    stream: boolean;
    /** Phantom — never invoked. */
    __t: (x: T) => T;
}
interface HeaderDescriptor { source: "header"; name: string }
interface QueryDescriptor { source: "query"; name: string }
interface ParamDescriptor { source: "param"; name: string }
// Same phantom pattern as BodyDescriptor<T> — TBody makes the type invariant so
// ExtractValue<ContextDescriptor<infer T>> recovers the precise body type.
interface ContextDescriptor<TBody = unknown> {
    source: "context";
    /** Phantom — never invoked. */
    __bodyType: (x: TBody) => TBody;
}

// `BodyDescriptor<any>` widens the union so any specific `BodyDescriptor<T>`
// (from Body<T>() or ZodBody) is assignable. Inference still recovers the
// precise T via `ExtractValue` below.
type ExtractDescriptor = BodyDescriptor<any> | HeaderDescriptor | QueryDescriptor | ParamDescriptor | ContextDescriptor<any>;

type ExtractValue<E> =
    E extends BodyDescriptor<infer T>    ? T :
    E extends ContextDescriptor<infer T> ? import("../runtime/context").RequestContext<T> :
    E extends ParamDescriptor            ? string :
    E extends QueryDescriptor            ? string | undefined :
    E extends HeaderDescriptor           ? string | undefined :
    never;

type ExtractContext<E> = {
    [K in keyof E]: ExtractValue<E[K]>;
};

type HttpMethod = "GET" | "POST" | "PATCH" | "PUT" | "DELETE";

interface InlineNativeMiddleware<T = unknown> {
    kind: "native";
    name: string;
    config?: T;
}

interface HandlerDefinition {
    method: HttpMethod;
    path: string;
    status: number;
    extract: Record<string, ExtractDescriptor>;
    validate: boolean;
    streaming: boolean;
    middleware?: (Function | InlineNativeMiddleware)[];
}

interface HandlerOptions {
    path: string;
    method: HttpMethod;
    status?: number;
    extract?: Record<string, ExtractDescriptor>;
    validate?: boolean;
    streaming?: boolean;
    middleware?: (Function | InlineNativeMiddleware)[];
}

interface SkyClassMetadata {
    handlers?: Map<string, HandlerDefinition>;
    group?: GroupDefinition;
    middleware?: MiddlewareDefinition;
    applyMiddleware?: Function[];
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
    ContextDescriptor,
    ExtractDescriptor,
    ExtractValue,
    ExtractContext,
    HttpMethod,
    HandlerDefinition,
    HandlerOptions,
    GroupDefinition,
    GroupOptions,
    MiddlewareDefinition,
    MiddlewareOptions,
    SkyClassMetadata,
    InlineNativeMiddleware,
};