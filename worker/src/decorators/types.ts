import type {RequestContext} from "../runtime";

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
    validate: boolean;
    /** Phantom — never invoked. */
    __t: (x: T) => T;
}
interface HeaderDescriptor { source: "header"; name: string }
interface QueryDescriptor { source: "query"; name: string }
interface ParamDescriptor { source: "param"; name: string }
// Same phantom pattern as BodyDescriptor<T> — TBody makes the type invariant so
// ExtractValue<ContextDescriptor<infer TBody>> recovers the precise body type.
interface ContextDescriptor<TBody = unknown> {
    source: "context";
    __t: (x: TBody) => TBody;
}

// `BodyDescriptor<any>` / `ContextDescriptor<any>` widen the unions so any
// specific generic variant is assignable. Inference still recovers the
// precise T via `ExtractValue` below.
type ExtractDescriptor = BodyDescriptor<any> | HeaderDescriptor | QueryDescriptor | ParamDescriptor | ContextDescriptor<any>;

type ExtractValue<E> =
    E extends BodyDescriptor<infer T>        ? T :
    E extends ContextDescriptor<infer TBody> ? RequestContext<TBody> :
    E extends ParamDescriptor                ? string :
    E extends QueryDescriptor                ? string | undefined :
    E extends HeaderDescriptor               ? string | undefined :
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
    streamRequestBody: boolean;
    streamResponseBody: boolean;
    middleware?: (Function | InlineNativeMiddleware)[];
    timeoutMs?: number;
}

interface HandlerOptions {
    path: string;
    method: HttpMethod;
    status?: number;
    extract?: Record<string, ExtractDescriptor>;
    validate?: boolean;
    streamResponseBody?: boolean;
    /** Alias for streamResponseBody — prefer this for readability. */
    streaming?: boolean;
    middleware?: (Function | InlineNativeMiddleware)[];
    /** Invocation timeout in milliseconds. Defaults to 30,000ms. */
    timeout?: number;
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