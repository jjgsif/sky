import ts from "typescript";

/**
 * JSON Schema representation.
 * Subset of JSON Schema draft 2020-12 sufficient for Sky's needs.
 */
export interface JsonSchema {
    type?: string | string[];
    properties?: Record<string, JsonSchema>;
    required?: string[];
    items?: JsonSchema;
    enum?: (string | number | boolean)[];
    const?: string | number | boolean;
    oneOf?: JsonSchema[];
    $ref?: string;
}

/**
 * Convert a TypeScript type to JSON Schema.
 *
 * This is the core algorithm. It recursively walks the type structure
 * and produces the equivalent JSON Schema representation.
 *
 * Supported types:
 *   - Primitives: string, number, boolean, null
 *   - Literal types: "admin", 42, true
 *   - Union types: "admin" | "member", string | null
 *   - Arrays: string[], Array<Foo>
 *   - Objects: interfaces and type aliases with properties
 *   - Optional properties: field?: T
 *   - Enums: TypeScript enum declarations
 *
 * Unsupported types throw with a descriptive error message.
 */
export function typeToJsonSchema(
    type: ts.Type,
    checker: ts.TypeChecker,
    seen?: Set<ts.Type>
): JsonSchema {
    const visited = seen ?? new Set();

    // Cycle detection for object types
    if (type.flags & ts.TypeFlags.Object) {
        if (visited.has(type)) {
            throw new Error(
                `Circular type detected: ${checker.typeToString(type)}. ` +
                `Sky does not support recursive types in Phase 2.`
            );
        }
        visited.add(type);
    }

    // --- Primitives ---

    // Boolean (must be before union check — boolean has Union flag)
    if (type.flags & ts.TypeFlags.Boolean) {
        return { type: "boolean" };
    }

    // Boolean literals (true, false individually)
    if (type.flags & ts.TypeFlags.BooleanLiteral) {
        return { type: "boolean" };
    }

    // Primitives
    if (type.flags & ts.TypeFlags.String) {
        return { type: "string" };
    }

    if (type.flags & ts.TypeFlags.Number) {
        return { type: "number" };
    }

    if (type.flags & ts.TypeFlags.Null) {
        return { type: "null" };
    }

    if (type.flags & ts.TypeFlags.Undefined) {
        return {};
    }

    // Literals
    if (type.isStringLiteral()) {
        return { const: type.value };
    }

    if (type.isNumberLiteral()) {
        return { const: type.value };
    }

    // Unions (after boolean check so boolean doesn't split)
    if (type.isUnion()) {
        return unionToSchema(type, checker, seen);
    }

    // --- Arrays ---

    if (isArrayType(type, checker)) {
        const typeArgs = getArrayElementType(type, checker);
        if (typeArgs) {
            return {
                type: "array",
                items: typeToJsonSchema(typeArgs, checker, seen),
            };
        }
    }

    // --- Object types (interfaces, type aliases, inline objects) ---

    if (type.flags & ts.TypeFlags.Object) {
        return objectToSchema(type, checker, seen);
    }

    // --- Unsupported ---

    const typeString = checker.typeToString(type);
    throw new Error(
        `Unsupported type: ${typeString}. ` +
        `Sky supports primitives, objects, arrays, unions, literals, and enums. ` +
        `Consider simplifying this type or providing an explicit Zod schema via @ZodBody.`
    );
}

/**
 * Convert a union type to JSON Schema.
 *
 * Special cases:
 *   - All string literals → enum
 *   - All number literals → enum
 *   - T | null → nullable type
 *   - Boolean (which TypeScript represents as true | false union)
 *   - Everything else → oneOf
 */
function unionToSchema(type: ts.UnionType, checker: ts.TypeChecker, seen?: Set<ts.Type>): JsonSchema {
    const variants = type.types;

    // Step 1: Filter out undefined (handled by optionality)
    const nonUndefined = variants.filter(
        t => !(t.flags & ts.TypeFlags.Undefined)
    );

    // Step 2: Separate null
    const nullVariants = nonUndefined.filter(
        t => t.flags & ts.TypeFlags.Null
    );
    const nonNull = nonUndefined.filter(
        t => !(t.flags & ts.TypeFlags.Null)
    );

    // Step 3: Collapse true | false into boolean
    const hasBooleanPair =
        nonNull.some(t => t.flags & ts.TypeFlags.BooleanLiteral && checker.typeToString(t) === "true") &&
        nonNull.some(t => t.flags & ts.TypeFlags.BooleanLiteral && checker.typeToString(t) === "false");

    // Build the effective variants with booleans collapsed
    const effective: JsonSchema[] = [];

    if (hasBooleanPair) {
        effective.push({ type: "boolean" });
    }

    for (const t of nonNull) {
        // Skip boolean literals if we already collapsed them
        if (hasBooleanPair && t.flags & ts.TypeFlags.BooleanLiteral) {
            continue;
        }
        effective.push(typeToJsonSchema(t, checker, seen));
    }

    // Step 4: Apply null if present
    const addNull = nullVariants.length > 0;

    // Single type remaining
    if (effective.length === 1 && !addNull) {
        return effective[0];
    }

    if (effective.length === 1 && addNull) {
        const inner = effective[0];
        if (typeof inner.type === "string") {
            return { ...inner, type: [inner.type, "null"] };
        }
        return { oneOf: [inner, { type: "null" }] };
    }

    // Check for all-string-literal enum
    if (effective.every(s => s.const !== undefined && typeof s.const === "string")) {
        const schema: JsonSchema = {
            type: "string",
            enum: effective.map(s => s.const as string),
        };
        if (addNull) schema.type = ["string", "null"];
        return schema;
    }

    // Check for all-number-literal enum
    if (effective.every(s => s.const !== undefined && typeof s.const === "number")) {
        const schema: JsonSchema = {
            type: "number",
            enum: effective.map(s => s.const as number),
        };
        if (addNull) schema.type = ["number", "null"];
        return schema;
    }

    // General oneOf
    if (addNull) {
        effective.push({ type: "null" });
    }
    return { oneOf: effective };
}

/**
 * Convert an object type to JSON Schema.
 *
 * Walks all properties, recursively converts their types,
 * and tracks which properties are required (non-optional).
 */
function objectToSchema(type: ts.Type, checker: ts.TypeChecker, seen?: Set<ts.Type>): JsonSchema {
    const properties: Record<string, JsonSchema> = {};
    const required: string[] = [];

    for (const prop of type.getProperties()) {
        // Get the type of this property
        const propType = checker.getTypeOfSymbolAtLocation(
            prop,
            prop.valueDeclaration!,
        );

        // Convert to JSON Schema recursively
        properties[prop.name] = typeToJsonSchema(propType, checker, seen);

        // Check if the property is optional (has ? modifier)
        if (!(prop.flags & ts.SymbolFlags.Optional)) {
            required.push(prop.name);
        }
    }

    return {
        type: "object",
        properties,
        ...(required.length > 0 ? { required } : {}),
    };
}

/**
 * Check if a type is an array type (T[] or Array<T>).
 */
function isArrayType(type: ts.Type, checker: ts.TypeChecker): boolean {
    // The type checker has a direct check for this
    if ((checker as any).isArrayType) {
        return (checker as any).isArrayType(type);
    }

    // Fallback: check if it's a reference to Array<T>
    if (type.flags & ts.TypeFlags.Object) {
        const objectType = type as ts.ObjectType;
        if (objectType.objectFlags & ts.ObjectFlags.Reference) {
            const ref = type as ts.TypeReference;
            const target = ref.target;
            if (target && target.symbol && target.symbol.name === "Array") {
                return true;
            }
        }
    }
    return false;
}

/**
 * Get the element type of an array type.
 */
function getArrayElementType(
    type: ts.Type,
    checker: ts.TypeChecker,
): ts.Type | undefined {
    if (type.flags & ts.TypeFlags.Object) {
        const ref = type as ts.TypeReference;
        const typeArgs = checker.getTypeArguments(ref);
        if (typeArgs.length > 0) {
            return typeArgs[0];
        }
    }
    return undefined;
}