import { describe, test, expect } from "bun:test";
import { createProgram, findTypeDeclarations } from "@/walker";
import { typeToJsonSchema, type JsonSchema } from "@/schema";
import path from "path";

const FIXTURES = path.join(import.meta.dir, "fixtures");

function getSchema(typeName: string): JsonSchema {
    const filePath = path.join(FIXTURES, "types.ts");
    const program = createProgram([filePath]);
    const checker = program.getTypeChecker();
    const sourceFile = program.getSourceFile(filePath);
    const declarations = findTypeDeclarations(sourceFile!, checker);

    const decl = declarations.find(d => d.name === typeName);
    if (!decl) throw new Error(`Type ${typeName} not found`);

    return typeToJsonSchema(decl.type, checker);
}

describe("typeToJsonSchema", () => {
    test("primitives", () => {
        const schema = getSchema("Primitives");
        expect(schema).toEqual({
            type: "object",
            properties: {
                name: { type: "string" },
                age: { type: "number" },
                active: { type: "boolean" },
            },
            required: ["name", "age", "active"],
        });
    });

    test("optional fields are not required", () => {
        const schema = getSchema("WithOptionals");
        expect(schema.required).toEqual(["required"]);
        expect(schema.properties!.optional).toEqual({ type: "number" });
    });

    test("nullable field", () => {
        const schema = getSchema("WithNullable");
        expect(schema.properties!.value).toEqual({
            type: ["string", "null"],
        });
        expect(schema.required).toEqual(["value"]);
    });

    test("string literal union becomes enum", () => {
        const schema = getSchema("WithRole");
        expect(schema.properties!.role).toEqual({
            type: "string",
            enum: ["admin", "member", "guest"],
        });
    });

    test("arrays", () => {
        const schema = getSchema("WithArrays");
        expect(schema.properties!.tags).toEqual({
            type: "array",
            items: { type: "string" },
        });
        expect(schema.properties!.scores).toEqual({
            type: "array",
            items: { type: "number" },
        });
    });

    test("nested objects", () => {
        const schema = getSchema("WithNested");
        expect(schema.properties!.address).toEqual({
            type: "object",
            properties: {
                street: { type: "string" },
                city: { type: "string" },
                zip: { type: "string" },
            },
            required: ["street", "city", "zip"],
        });
    });

    test("complex union becomes oneOf", () => {
        const schema = getSchema("WithUnion");
        expect(schema.properties!.id).toEqual({
            oneOf: [{ type: "string" }, { type: "number" }],
        });
    });

    test("complex type with mixed optional and nullable", () => {
        const schema = getSchema("Complex");
        expect(schema.required).toEqual(["required", "nullable"]);
        expect(schema.properties!.required).toEqual({ type: "string" });
        expect(schema.properties!.optional).toEqual({ type: "number" });
        expect(schema.properties!.nullable).toEqual({
            type: ["string", "null"],
        });
    });
});

describe("boolean edge cases", () => {
    test("plain boolean", () => {
        const schema = getSchema("BooleanEdges");
        expect(schema.properties!.plain).toEqual({ type: "boolean" });
    });

    test("literal true", () => {
        const schema = getSchema("BooleanEdges");
        // true as a literal could be { const: true } or { type: "boolean" }
        // depending on how strict we want to be
        const prop = schema.properties!.literal;
        expect(
            prop.type === "boolean" || prop.const === true
        ).toBe(true);
    });

    test("nullable boolean", () => {
        const schema = getSchema("BooleanEdges");
        expect(schema.properties!.nullable).toEqual({
            type: ["boolean", "null"],
        });
    });

    test("optional boolean is not required", () => {
        const schema = getSchema("BooleanEdges");
        expect(schema.required).not.toContain("optional");
        expect(schema.properties!.optional).toEqual({ type: "boolean" });
    });

    test("optional nullable boolean", () => {
        const schema = getSchema("BooleanEdges");
        expect(schema.required).not.toContain("optionalNullable");
        expect(schema.properties!.optionalNullable).toEqual({
            type: ["boolean", "null"],
        });
    });
});

describe("array edge cases", () => {
    test("nested arrays (matrix)", () => {
        const schema = getSchema("NestedArrays");
        expect(schema.properties!.matrix).toEqual({
            type: "array",
            items: { type: "array", items: { type: "number" } },
        });
    });

    test("array of objects", () => {
        const schema = getSchema("NestedArrays");
        const items = schema.properties!.objectArray;
        expect(items.type).toBe("array");
        expect(items.items!.type).toBe("object");
        expect(items.items!.properties!.id).toEqual({ type: "string" });
        expect(items.items!.properties!.value).toEqual({ type: "number" });
    });

    test("optional array", () => {
        const schema = getSchema("NestedArrays");
        expect(schema.required).not.toContain("optionalArray");
        expect(schema.properties!.optionalArray).toEqual({
            type: "array",
            items: { type: "string" },
        });
    });

    test("nullable array", () => {
        const schema = getSchema("NestedArrays");
        const prop = schema.properties!.nullableArray;
        // Should be either { type: ["array", "null"], items: ... }
        // or { oneOf: [{ type: "array", items: ... }, { type: "null" }] }
        expect(
            prop.type?.includes("null") || prop.oneOf?.some(s => s.type === "null")
        ).toBe(true);
    });
});

describe("deeply nested objects", () => {
    test("inline nested object types", () => {
        const schema = getSchema("DeepNested");
        expect(schema.properties!.level1.type).toBe("object");
        expect(schema.properties!.level1.properties!.level2.type).toBe("object");
        expect(
            schema.properties!.level1.properties!.level2.properties!.value
        ).toEqual({ type: "string" });
    });
});

describe("mixed unions", () => {
    test("string or number", () => {
        const schema = getSchema("MixedUnions");
        expect(schema.properties!.stringOrNumber).toEqual({
            oneOf: [{ type: "string" }, { type: "number" }],
        });
    });

    test("triple union", () => {
        const schema = getSchema("MixedUnions");
        const prop = schema.properties!.tripleUnion;
        expect(prop.oneOf).toBeDefined();
        expect(prop.oneOf!.length).toBe(3);
    });

    test("literal mixed with type", () => {
        const schema = getSchema("MixedUnions");
        const prop = schema.properties!.literalOrType;
        // "specific" | number → oneOf with const and type
        expect(prop.oneOf).toBeDefined();
    });
});

describe("enums", () => {
    test("string enum", () => {
        const schema = getSchema("WithEnums");
        const prop = schema.properties!.status;
        expect(prop.enum).toEqual(["active", "inactive", "pending"]);
    });

    test("numeric enum", () => {
        const schema = getSchema("WithEnums");
        const prop = schema.properties!.priority;
        expect(prop.enum).toEqual([0, 1, 2]);
    });
});

describe("empty and minimal", () => {
    test("empty interface", () => {
        const schema = getSchema("Empty");
        expect(schema).toEqual({
            type: "object",
            properties: {},
        });
    });

    test("single field", () => {
        const schema = getSchema("SingleField");
        expect(schema).toEqual({
            type: "object",
            properties: { only: { type: "string" } },
            required: ["only"],
        });
    });
});

describe("all optional", () => {
    test("no required array when all fields optional", () => {
        const schema = getSchema("AllOptional");
        expect(schema.required).toBeUndefined();
        expect(Object.keys(schema.properties!).length).toBe(3);
    });
});

describe("number literals", () => {
    test("number literal union becomes enum", () => {
        const schema = getSchema("NumberLiterals");
        expect(schema.properties!.httpStatus).toEqual({
            type: "number",
            enum: [200, 201, 204, 400, 500],
        });
    });
});

describe("type aliases", () => {
    test("string alias resolves to string", () => {
        const schema = getSchema("WithAliases");
        expect(schema.properties!.aliasedString).toEqual({ type: "string" });
    });

    test("object alias resolves to object", () => {
        const schema = getSchema("WithAliases");
        const prop = schema.properties!.aliasedObject;
        expect(prop.type).toBe("object");
        expect(prop.properties!.foo).toEqual({ type: "string" });
        expect(prop.properties!.bar).toEqual({ type: "number" });
    });

    test("union alias resolves to enum", () => {
        const schema = getSchema("WithAliases");
        expect(schema.properties!.aliasedUnion).toEqual({
            type: "string",
            enum: ["a", "b", "c"],
        });
    });

    test("nullable alias resolves correctly", () => {
        const schema = getSchema("WithAliases");
        expect(schema.properties!.aliasedNullable).toEqual({
            type: ["string", "null"],
        });
    });
});

describe("recursive types", () => {
    test("recursive type should error with clear message", () => {
        expect(() => getSchema("TreeNode")).toThrow();
    });
});