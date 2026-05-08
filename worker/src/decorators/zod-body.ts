import { type BodyDescriptor } from "./types";
import { type ZodType, toJSONSchema } from "zod";
import type {ZodStandardJSONSchemaPayload} from "zod/v4/core";


interface ZodBodyDescriptor<T = unknown> extends BodyDescriptor<T> {
    schema: ZodType,
    jsonSchema: ReturnType<typeof toJSONSchema<ZodType>>
}

const ZodBody = <S extends ZodType>(schema: S): {
    source: string;
    stream: boolean;
    schema: S;
    jsonSchema: ZodStandardJSONSchemaPayload<S>
} => ({
    source: "body",
    stream: false,
    schema,
    jsonSchema: toJSONSchema(schema),
});

export { ZodBody };