import { BodyDescriptor, HeaderDescriptor, QueryDescriptor, ParamDescriptor } from "./types";

function Body(): BodyDescriptor { return { source: "body" }; }
function Header(name: string): HeaderDescriptor { return { source: "header", name }; }
function Query(name: string): QueryDescriptor { return { source: "query", name }; }
function Param(name: string): ParamDescriptor { return { source: "param", name }; }

export {
    Body,
    Header,
    Query,
    Param
};