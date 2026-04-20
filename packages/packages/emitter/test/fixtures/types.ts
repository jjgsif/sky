// Primitives
interface Primitives {
    name: string;
    age: number;
    active: boolean;
}

// Optional fields
interface WithOptionals {
    required: string;
    optional?: number;
}

// Nullable
interface WithNullable {
    value: string | null;
}

// String literal union (enum-like)
interface WithRole {
    role: "admin" | "member" | "guest";
}

// Arrays
interface WithArrays {
    tags: string[];
    scores: number[];
}

// Nested objects
interface Address {
    street: string;
    city: string;
    zip: string;
}

interface WithNested {
    name: string;
    address: Address;
}

// Complex union
interface WithUnion {
    id: string | number;
}

// Optional + nullable
interface Complex {
    required: string;
    optional?: number;
    nullable: string | null;
    optionalNullable?: boolean | null;
}

// === Edge Cases: Booleans ===
interface BooleanEdges {
    plain: boolean;
    literal: true;
    nullable: boolean | null;
    optional?: boolean;
    optionalNullable?: boolean | null;
}

// === Edge Cases: Nested Arrays ===
interface NestedArrays {
    matrix: number[][];
    objectArray: SimpleItem[];
    optionalArray?: string[];
    nullableArray: number[] | null;
}

interface SimpleItem {
    id: string;
    value: number;
}

// === Edge Cases: Deeply Nested Objects ===
interface DeepNested {
    level1: {
        level2: {
            value: string;
        };
    };
}

// === Edge Cases: Mixed Unions ===
interface MixedUnions {
    stringOrNumber: string | number;
    stringOrNull: string | null;
    tripleUnion: string | number | boolean;
    literalOrType: "specific" | number;
}

// === Edge Cases: Enums ===
enum Status {
    Active = "active",
    Inactive = "inactive",
    Pending = "pending",
}

enum NumericPriority {
    Low = 0,
    Medium = 1,
    High = 2,
}

interface WithEnums {
    status: Status;
    priority: NumericPriority;
}

// === Edge Cases: Empty and Minimal ===
interface Empty {}

interface SingleField {
    only: string;
}

// === Edge Cases: All Optional ===
interface AllOptional {
    a?: string;
    b?: number;
    c?: boolean;
}

// === Edge Cases: Number Literals ===
interface NumberLiterals {
    httpStatus: 200 | 201 | 204 | 400 | 500;
}

// === Edge Cases: Mixed Literal Union ===
interface MixedLiterals {
    code: "ok" | "error";
    count: 1 | 2 | 3;
}

// === Edge Cases: Type Aliases ===
type StringAlias = string;
type ObjectAlias = {
    foo: string;
    bar: number;
};
type UnionAlias = "a" | "b" | "c";
type NullableString = string | null;

interface WithAliases {
    aliasedString: StringAlias;
    aliasedObject: ObjectAlias;
    aliasedUnion: UnionAlias;
    aliasedNullable: NullableString;
}

// === Edge Cases: Recursive (should error) ===
interface TreeNode {
    value: string;
    children: TreeNode[];
}

export type {
    Primitives,
    WithOptionals,
    WithNullable,
    WithRole,
    WithArrays,
    Address,
    WithNested,
    WithUnion,
    Complex,
    BooleanEdges,
    NestedArrays,
    SimpleItem,
    DeepNested,
    MixedUnions,
    Status,
    NumericPriority,
    WithEnums,
    Empty,
    SingleField,
    AllOptional,
    NumberLiterals,
    MixedLiterals,
    StringAlias,
    ObjectAlias,
    UnionAlias,
    NullableString,
    WithAliases,
    TreeNode
};