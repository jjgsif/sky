import { type BodyDescriptor } from "@decorators";
import { type ZodType, toJSONSchema } from "zod";


interface ZodBodyDescriptor extends BodyDescriptor {
    schema: ZodType,
    jsonSchema: ReturnType<typeof toJSONSchema<ZodType>>
}

const ZodBody = (schema: ZodType): ZodBodyDescriptor => ({ source: "body", schema, jsonSchema: toJSONSchema(schema) });

export { ZodBody };