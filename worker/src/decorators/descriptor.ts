import type { BodyDescriptor, HeaderDescriptor, QueryDescriptor, ParamDescriptor } from "./types";

// `phantom` is a never-invoked function used only to carry T at the type
// level. The runtime closure is harmless (single allocation per descriptor).
const phantom = <T>(x: T): T => x;

// Body<T>() lets the user lift a type into the descriptor for inference, e.g.
//   const extract = { body: Body<{ name: string }>() } as const;
// `ExtractContext<typeof extract>["body"]` then resolves to `{ name: string }`.
function Body<T = unknown>(): BodyDescriptor<T> {
    return { source: "body", stream: false, __t: phantom as (x: T) => T };
}
function StreamedBody(): BodyDescriptor<AsyncIterable<Uint8Array>> {
    return { source: "body", stream: true, __t: phantom as (x: AsyncIterable<Uint8Array>) => AsyncIterable<Uint8Array> };
}
function Header(name: string): HeaderDescriptor { return { source: "header", name }; }
function Query(name: string): QueryDescriptor { return { source: "query", name }; }
function Param(name: string): ParamDescriptor { return { source: "param", name }; }

export {
    Body,
    StreamedBody,
    Header,
    Query,
    Param
};