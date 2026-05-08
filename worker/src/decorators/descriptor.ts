import type { BodyDescriptor, HeaderDescriptor, QueryDescriptor, ParamDescriptor } from "./types";

function Body(): BodyDescriptor { return { source: "body", stream: false }; }
function StreamedBody(): BodyDescriptor { return { source: "body", stream: true } };
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