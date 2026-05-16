import { type BodyDescriptor } from "./types";
import { type ZodType, toJSONSchema, type infer as ZodInfer } from "zod";
import type { ZodStandardJSONSchemaPayload } from "zod/v4/core";

interface ZodBodyDescriptor<T> extends BodyDescriptor<T> {
    schema: ZodType;
    jsonSchema: ReturnType<typeof toJSONSchema<ZodType>>;
}

const phantom = <T>(x: T): T => x;

const ZodBody = <S extends ZodType>(
    schema: S,
): ZodBodyDescriptor<ZodInfer<S>> & {
    jsonSchema: ZodStandardJSONSchemaPayload<S>;
} => ({
    source: "body",
    stream: false,
    validate: true,
    __t: phantom as (x: ZodInfer<S>) => ZodInfer<S>,
    schema,
    jsonSchema: toJSONSchema(schema),
});

export { ZodBody };
