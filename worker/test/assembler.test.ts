import { describe, test, expect, beforeAll } from "bun:test";
import { assembleManifest, type ManifestOutput } from "@sky/emitter";
import path from "path";

const APP_DIR = path.join(import.meta.dir, "fixtures", "app");
let manifest: ManifestOutput;

beforeAll(async () => {
    manifest = await assembleManifest(
        [path.join(APP_DIR, "index.ts")],
        path.join(APP_DIR, "sky-manifest.json"),
    );
});

describe("manifest structure", () => {
    test("has version 1", () => {
        expect(manifest.version).toBe("1");
    });

    test("has a hash", () => {
        expect(manifest.hash).toStartWith("sha256:");
        expect(manifest.hash.length).toBeGreaterThan(10);
    });

    test("has emitted_at timestamp", () => {
        expect(manifest.emitted_at).toBeDefined();
        expect(new Date(manifest.emitted_at).getTime()).not.toBeNaN();
    });

    test("has schemas section", () => {
        expect(manifest.schemas).toBeDefined();
        expect(typeof manifest.schemas).toBe("object");
    });
});

describe("services", () => {
    test("discovers all services", () => {
        const names = manifest.services.map(s => s.name);
        expect(names).toContain("UserService");
        expect(names).toContain("HealthService");
        expect(names).toContain("DatabaseClient");
        expect(names).toContain("AuthMiddleware");
    });

    test("UserService has correct lifetime", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        expect(user!.lifetime).toBe("scoped");
    });

    test("DatabaseClient is singleton", () => {
        const db = manifest.services.find(s => s.name === "DatabaseClient");
        expect(db!.lifetime).toBe("singleton");
    });

    test("HealthService is singleton", () => {
        const health = manifest.services.find(s => s.name === "HealthService");
        expect(health!.lifetime).toBe("singleton");
    });
});

describe("dependencies", () => {
    test("UserService depends on DatabaseClient", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        expect(user!.dependencies.length).toBe(1);
        expect(user!.dependencies[0].type).toBe("class");
        expect(user!.dependencies[0].value).toBe("DatabaseClient");
        expect(user!.dependencies[0].position).toBe(0);
    });

    test("DatabaseClient has no dependencies", () => {
        const db = manifest.services.find(s => s.name === "DatabaseClient");
        expect(db!.dependencies.length).toBe(0);
    });
});

describe("handlers", () => {
    test("UserService has five handlers", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        expect(user!.handlers.length).toBe(5);
    });

    test("createUser handler has correct config", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "createUser");

        expect(handler).toBeDefined();
        expect(handler!.method).toBe("POST");
        expect(handler!.path).toBe("/");
        expect(handler!.status).toBe(201);
        expect(handler!.validate).toBe(true);
    });

    test("createUser has Body and Header extracts", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "createUser");

        const byField = Object.fromEntries(handler!.extract.map(e => [e.field, e]));
        expect(handler!.extract.length).toBe(2);
        expect(byField["body"]!.source).toBe("body");
        expect(byField["tenantId"]!.source).toBe("header");
        expect(byField["tenantId"]!.name).toBe("x-tenant-id");
    });

    test("getUser has Param extract", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "getUser");

        expect(handler!.extract.length).toBe(1);
        expect(handler!.extract[0].field).toBe("id");
        expect(handler!.extract[0].source).toBe("param");
        expect(handler!.extract[0].name).toBe("id");
    });

    test("listUsers has Query extracts", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "listUsers");

        const byField = Object.fromEntries(handler!.extract.map(e => [e.field, e]));
        expect(handler!.extract.length).toBe(2);
        expect(byField["page"]!.source).toBe("query");
        expect(byField["page"]!.name).toBe("page");
        expect(byField["limit"]!.source).toBe("query");
        expect(byField["limit"]!.name).toBe("limit");
    });

    test("updateUser has Param and Body extracts", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "updateUser");

        const byField = Object.fromEntries(handler!.extract.map(e => [e.field, e]));
        expect(handler!.extract.length).toBe(2);
        expect(byField["id"]!.source).toBe("param");
        expect(byField["id"]!.name).toBe("id");
        expect(byField["body"]!.source).toBe("body");
    });

    test("deleteUser has validate false", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "deleteUser");

        expect(handler!.validate).toBe(false);
        expect(handler!.status).toBe(204);
    });

    test("deleteUser has no response schema", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "deleteUser");

        expect(handler!.response).toBeUndefined();
    });

    test("HealthService has one handler", () => {
        const health = manifest.services.find(s => s.name === "HealthService");
        expect(health!.handlers.length).toBe(1);
        expect(health!.handlers[0].method).toBe("GET");
        expect(health!.handlers[0].path).toBe("/health");
    });

    test("service with no handlers has empty array", () => {
        const db = manifest.services.find(s => s.name === "DatabaseClient");
        expect(db!.handlers.length).toBe(0);
    });
});

describe("schemas", () => {
    test("CreateUserInput schema exists", () => {
        expect(manifest.schemas["CreateUserInput"]).toBeDefined();
    });

    test("CreateUserInput has correct structure", () => {
        const schema = manifest.schemas["CreateUserInput"];
        expect(schema.type).toBe("object");
        expect(schema.properties!.email).toEqual({ type: "string" });
        expect(schema.properties!.name).toEqual({ type: "string" });
        expect(schema.properties!.role).toEqual({
            type: "string",
            enum: ["admin", "member"],
        });
        expect(schema.required).toContain("email");
        expect(schema.required).toContain("name");
        expect(schema.required).toContain("role");
    });

    test("UserResponse schema exists", () => {
        expect(manifest.schemas["UserResponse"]).toBeDefined();
    });

    test("UpdateUserInput has optional fields", () => {
        const schema = manifest.schemas["UpdateUserInput"];
        expect(schema).toBeDefined();
        // Both fields are optional so required should be absent or empty
        expect(schema.required).toBeUndefined();
    });

    test("createUser body extract references CreateUserInput schema", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "createUser");
        const bodyExtract = handler!.extract.find(e => e.source === "body");

        expect(bodyExtract!.schema).toBeDefined();
        expect(bodyExtract!.schema!.$ref).toBe("#/schemas/CreateUserInput");
    });

    test("createUser response references UserResponse schema", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        const handler = user!.handlers.find(h => h.name === "createUser");

        expect(handler!.response).toBeDefined();
        expect(handler!.response!.$ref).toBe("#/schemas/UserResponse");
    });

    test("shared types are deduplicated", () => {
        // UserResponse is used by createUser, getUser, updateUser
        // It should appear once in schemas
        const responseRefs = manifest.services
            .flatMap(s => s.handlers)
            .filter(h => h.response?.$ref === "#/schemas/UserResponse");

        expect(responseRefs.length).toBeGreaterThan(1);
        expect(manifest.schemas["UserResponse"]).toBeDefined();
    });
});

describe("groups", () => {
    test("UserService has group with prefix", () => {
        const user = manifest.services.find(s => s.name === "UserService");
        expect(user!.group).toBeDefined();
        expect(user!.group!.prefix).toBe("/api/v1/users");
    });

    test("HealthService has no group", () => {
        const health = manifest.services.find(s => s.name === "HealthService");
        expect(health!.group).toBeUndefined();
    });
});

describe("middleware", () => {
    test("AuthMiddleware is in middleware list", () => {
        const auth = manifest.middleware.find(m => m.name === "AuthMiddleware");
        expect(auth).toBeDefined();
        expect(auth!.global).toBe(true);
        expect(auth!.order).toBe(1);
        expect(auth!.kind).toBe("user");
    });
});

describe("manifest file output", () => {
    test("sky-manifest.json was written", () => {
        const fs = require("fs");
        const outputPath = path.join(APP_DIR, "sky-manifest.json");
        expect(fs.existsSync(outputPath)).toBe(true);
    });

    test("written file matches returned manifest", () => {
        const fs = require("fs");
        const outputPath = path.join(APP_DIR, "sky-manifest.json");
        const written = JSON.parse(fs.readFileSync(outputPath, "utf-8"));
        expect(written.version).toBe(manifest.version);
        expect(written.hash).toBe(manifest.hash);
    });
});