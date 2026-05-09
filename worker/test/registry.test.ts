import { describe, test, expect } from "bun:test";
import { createProgram, findTypeDeclarations } from "@sky/emitter/walker";
import { typeToJsonSchema, type JsonSchema } from "@sky/emitter/schema";
import { SchemaRegistry } from "@sky/emitter/registry";
import path from "path";

const FIXTURES = path.join(import.meta.dir, "fixtures");

function createTestContext() {
    const filePath = path.join(FIXTURES, "types.ts");
    const program = createProgram([filePath]);
    const checker = program.getTypeChecker();
    const sourceFile = program.getSourceFile(filePath);
    const declarations = findTypeDeclarations(sourceFile!, checker);
    const registry = new SchemaRegistry();

    return { checker, declarations, registry };
}

function getType(declarations: any[], name: string) {
    const decl = declarations.find((d: any) => d.name === name);
    if (!decl) throw new Error(`Type ${name} not found in fixtures`);
    return decl;
}

describe("schema registry", () => {
    test("shared type produces $ref instead of inline", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");

        const ref = typeToJsonSchema(person.type, checker, registry);

        // The return value is a $ref to the registered schema
        expect(ref).toEqual({ $ref: "#/schemas/PersonWithAddress" });

        // Look up the actual schema in the registry
        const schemas = registry.getSchemas();
        const personSchema = schemas["PersonWithAddress"];

        // home should be a $ref to SharedAddress, not inlined
        expect(personSchema.properties!.home).toEqual({
            $ref: "#/schemas/SharedAddress",
        });
    });

    test("$ref points to schemas section entry", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");

        typeToJsonSchema(person.type, checker, registry);

        const schemas = registry.getSchemas();
        expect(schemas["SharedAddress"]).toBeDefined();
        expect(schemas["SharedAddress"].type).toBe("object");
        expect(schemas["SharedAddress"].properties!.street).toEqual({
            type: "string",
        });
        expect(schemas["SharedAddress"].properties!.city).toEqual({
            type: "string",
        });
    });

    test("same type referenced twice produces one schema entry", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");
        const company = getType(declarations, "CompanyWithAddress");

        // Walk both types — both reference SharedAddress
        typeToJsonSchema(person.type, checker, registry);
        typeToJsonSchema(company.type, checker, registry);

        const schemas = registry.getSchemas();

        // SharedAddress should appear exactly once
        const addressEntries = Object.keys(schemas).filter(
            (k) => k === "SharedAddress"
        );
        expect(addressEntries.length).toBe(1);

        // Both parent types should reference it via $ref
        const personSchema = schemas["PersonWithAddress"];
        const companySchema = schemas["CompanyWithAddress"];

        expect(personSchema.properties!.home).toEqual({
            $ref: "#/schemas/SharedAddress",
        });
        expect(companySchema.properties!.headquarters).toEqual({
            $ref: "#/schemas/SharedAddress",
        });
    });

    test("recursive type produces valid $ref cycle", () => {
        const { checker, declarations, registry } = createTestContext();
        const treeNode = getType(declarations, "TreeNode");

        // Should not throw — recursive types are supported via $ref
        const ref = typeToJsonSchema(treeNode.type, checker, registry);

        // The return value should be a $ref to TreeNode
        expect(ref).toEqual({ $ref: "#/schemas/TreeNode" });

        // The schema should reference itself in children.items
        const schemas = registry.getSchemas();
        const treeSchema = schemas["TreeNode"];

        expect(treeSchema.type).toBe("object");
        expect(treeSchema.properties!.value).toEqual({ type: "string" });
        expect(treeSchema.properties!.children).toEqual({
            type: "array",
            items: { $ref: "#/schemas/TreeNode" },
        });
    });

    test("anonymous inline objects are inlined, not registered", () => {
        const { checker, declarations, registry } = createTestContext();
        const withInline = getType(declarations, "WithInlineObject");

        typeToJsonSchema(withInline.type, checker, registry);

        const schemas = registry.getSchemas();

        // The anonymous metadata object should NOT be in the registry
        // Only named types get registered
        expect(schemas["__type"]).toBeUndefined();

        // But it should be inlined in the parent schema
        const parentSchema = schemas["WithInlineObject"];
        expect(parentSchema.properties!.metadata.type).toBe("object");
        expect(parentSchema.properties!.metadata.properties!.createdAt).toEqual({
            type: "string",
        });
    });

    test("zod override replaces type-derived schema", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");

        // First, walk PersonWithAddress so SharedAddress gets registered
        typeToJsonSchema(person.type, checker, registry);

        // Verify the type-derived schema is there
        let schemas = registry.getSchemas();
        expect(schemas["SharedAddress"].properties!.street).toEqual({
            type: "string",
        });

        // Now override with a Zod-derived schema
        const zodSchema: JsonSchema = {
            type: "object",
            properties: {
                //@ts-ignore
                street: { type: "string", minLength: 1 },
                //@ts-ignore
                city: { type: "string", minLength: 1 },
                //@ts-ignore
                zip: { type: "string", pattern: "^[0-9]{5}$" },
            },
            required: ["street", "city", "zip"],
        };

        registry.registerZodOverride("SharedAddress", zodSchema);

        // The override should replace the original
        schemas = registry.getSchemas();
        expect(schemas["SharedAddress"]).toEqual(zodSchema);
        expect(schemas["SharedAddress"].properties!.zip).toBeDefined();
    });

    test("getSchemas returns all registered schemas", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");
        const article = getType(declarations, "Article");

        typeToJsonSchema(person.type, checker, registry);
        typeToJsonSchema(article.type, checker, registry);

        const schemas = registry.getSchemas();

        // Should contain all named types encountered during walking
        expect(schemas["PersonWithAddress"]).toBeDefined();
        expect(schemas["SharedAddress"]).toBeDefined();
        expect(schemas["Article"]).toBeDefined();
        expect(schemas["Tag"]).toBeDefined();
    });

    test("deeply nested shared types are all registered", () => {
        const { checker, declarations, registry } = createTestContext();
        const feed = getType(declarations, "Feed");

        typeToJsonSchema(feed.type, checker, registry);

        const schemas = registry.getSchemas();

        // Feed → Article → Tag, all should be registered
        expect(schemas["Feed"]).toBeDefined();
        expect(schemas["Article"]).toBeDefined();
        expect(schemas["Tag"]).toBeDefined();

        // Feed.articles should ref Article
        expect(schemas["Feed"].properties!.articles).toEqual({
            type: "array",
            items: { $ref: "#/schemas/Article" },
        });

        // Article.tags should ref Tag
        expect(schemas["Article"].properties!.tags).toEqual({
            type: "array",
            items: { $ref: "#/schemas/Tag" },
        });
    });

    test("parent type is also registered when walked with registry", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");

        // Walking PersonWithAddress should register both PersonWithAddress and SharedAddress
        const ref = typeToJsonSchema(person.type, checker, registry);

        // The return should be a $ref to the parent type itself
        expect(ref).toEqual({ $ref: "#/schemas/PersonWithAddress" });

        const schemas = registry.getSchemas();
        expect(schemas["PersonWithAddress"]).toBeDefined();
        expect(schemas["PersonWithAddress"].properties!.name).toEqual({
            type: "string",
        });
    });

    test("registry does not re-walk already registered types", () => {
        const { checker, declarations, registry } = createTestContext();
        const person = getType(declarations, "PersonWithAddress");

        // Walk twice
        const ref1 = typeToJsonSchema(person.type, checker, registry);
        const ref2 = typeToJsonSchema(person.type, checker, registry);

        // Both should return the same $ref
        expect(ref1).toEqual(ref2);
        expect(ref1).toEqual({ $ref: "#/schemas/PersonWithAddress" });

        // Schema should exist exactly once
        const schemas = registry.getSchemas();
        expect(Object.keys(schemas).filter(k => k === "PersonWithAddress").length).toBe(1);
    });
});