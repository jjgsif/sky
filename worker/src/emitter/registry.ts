import ts from "typescript";
import { typeToJsonSchema, objectToSchema, type JsonSchema } from "./schema";

export class SchemaRegistry {
    private schemas = new Map<string, JsonSchema>();

    register(
        name: string,
        type: ts.Type,
        checker: ts.TypeChecker,
        seen?: Set<ts.Type>,
    ): JsonSchema {
        // Already registered — return ref
        if (this.schemas.has(name)) {
            return { $ref: `#/schemas/${name}` };
        }

        // Placeholder for cycle protection
        this.schemas.set(name, {});

        // Walk the type
        const schema = objectToSchema(type, checker, this, seen);

        // Replace placeholder
        this.schemas.set(name, schema);

        return { $ref: `#/schemas/${name}` };
    }

    registerZodOverride(name: string, jsonSchema: JsonSchema): void {
        this.schemas.set(name, jsonSchema);
    }

    getSchemas(): Record<string, JsonSchema> {
        return Object.fromEntries(this.schemas);
    }
}