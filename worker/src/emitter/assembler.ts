export interface ManifestOutput {
    version: "1";
    hash: string;
    emitted_at: string;
    services: ManifestService[];
    middleware: ManifestMiddleware[];
    schemas: Record<string, JsonSchema>;
}

export interface ManifestMiddlewareEntry {
    kind: "native" | "user";
    name: string;
    config?: unknown;
}

export interface ManifestService {
    name: string;
    className: string;
    lifetime: "singleton" | "scoped" | "transient";
    dependencies: ManifestDependency[];
    handlers: ManifestHandler[];
    group?: ManifestGroup;
    middleware?: ManifestMiddlewareEntry[];
}

export interface ManifestDependency {
    type: "class" | "string" | "symbol";
    value: string;
    position: number;
}

export interface ManifestHandler {
    name: string;
    method: string;
    path: string;
    status: number;
    validate: boolean;
    streaming: boolean;
    extract: ManifestExtract[];
    response?: JsonSchema;
    middleware?: ManifestMiddlewareEntry[];
}

export interface ManifestExtract {
    /** Field name in the handler's input object (the key in `extract: { ... }`). */
    field: string;
    source: "body" | "query" | "param" | "header";
    /** Source-side name (header name, query/param key). Absent for body. */
    name?: string;
    schema?: JsonSchema
}

export interface ManifestMiddleware {
    name: string;
    className: string;
    global: boolean;
    order: number;
    kind: "user" | "native";
    config?: unknown;
}

export interface ManifestGroup {
    prefix: string;
    middleware: ManifestMiddlewareEntry[];
}

import ts from "typescript";
import { createProgram } from "./walker";
import { typeToJsonSchema, type JsonSchema } from "./schema";
import { SchemaRegistry } from "./registry";
import { ServiceMap } from "../decorators";
import type { ServiceRegistration } from "../decorators";
import { getNativeMiddleware } from "./native-middleware";
import { createHash } from "crypto";
import fs from "fs";
import path from "path";

export async function assembleManifest(
    entryPoints: string[],
    outputPath: string,
): Promise<ManifestOutput> {
    // Phase A: Import user modules to trigger decorators
    for (const entry of entryPoints) {
        await import(path.resolve(entry));
    }

    // Phase B: Create TS program for type analysis
    const program = createProgram(entryPoints);
    const checker = program.getTypeChecker();
    const registry = new SchemaRegistry();

    // Build the manifest from ServiceMap + type analysis
    const services: ManifestService[] = [];
    const middleware: ManifestMiddleware[] = [];

    for (const [constructor, registration] of ServiceMap.entries()) {
        if (typeof constructor === "string") continue;

        const className = constructor.name;

        // Find the class in the AST
        const classNode = findClassDeclaration(program, className);

        // Process handlers
        const handlers = processHandlers(
            registration,
            classNode,
            checker,
            registry,
        );

        // Process dependencies
        const dependencies = processDependencies(registration);

        // Build the service entry
        const service: ManifestService = {
            name: registration.name ?? className,
            className,
            lifetime: registration.lifetime,
            dependencies,
            handlers,
        };

        // Attach group if present
        if (registration.group) {
            service.group = {
                prefix: registration.group.prefix,
                middleware: registration.group.middleware.map(
                    (m: Function): ManifestMiddlewareEntry => ({ kind: "user", name: m.name })
                ),
            };
        }

        // Service-level applied middleware (from @Middleware(Cls) on the service class)
        if (registration.applyMiddleware?.length) {
            service.middleware = registration.applyMiddleware.map(
                (m: Function): ManifestMiddlewareEntry => ({ kind: "user", name: m.name })
            );
        }

        services.push(service);

        // If this service is also middleware, add to middleware list
        if (registration.middleware) {
            middleware.push({
                name: registration.name ?? className,
                className,
                global: registration.middleware.global,
                order: registration.middleware.order,
                kind: registration.middleware.kind,
            });
        }
    }

    // Append native (gateway-side) middleware registered via globalMiddleware()
    for (const nm of getNativeMiddleware()) {
        middleware.push({
            name: nm.name,
            className: nm.name,
            global: nm.global,
            order: nm.order,
            kind: "native",
            config: nm.config,
        });
    }

    // Assemble final manifest
    const manifest: ManifestOutput = {
        version: "1",
        hash: "",
        emitted_at: new Date().toISOString(),
        services,
        middleware,
        schemas: registry.getSchemas(),
    };

    // Compute hash from content (excluding hash and emitted_at)
    manifest.hash = computeHash(manifest);

    // Write atomically
    writeManifestAtomic(outputPath, manifest);

    return manifest;
}

function findClassDeclaration(
    program: ts.Program,
    className: string,
): ts.ClassDeclaration | undefined {
    for (const sourceFile of program.getSourceFiles()) {
        // Skip declaration files (node_modules, lib.d.ts, etc.)
        if (sourceFile.isDeclarationFile) continue;

        let found: ts.ClassDeclaration | undefined;
        ts.forEachChild(sourceFile, (node) => {
            if (ts.isClassDeclaration(node) && node.name?.text === className) {
                found = node;
            }
        });
        if (found) return found;
    }
    return undefined;
}

function findMethodDeclaration(
    classNode: ts.ClassDeclaration,
    methodName: string,
): ts.MethodDeclaration | undefined {
    let found: ts.MethodDeclaration | undefined;
    ts.forEachChild(classNode, (node) => {
        if (ts.isMethodDeclaration(node)) {
            const name = node.name;
            if (ts.isIdentifier(name) && name.text === methodName) {
                found = node;
            }
        }
    });
    return found;
}

function toMiddlewareEntry(m: Function | { kind: "native"; name: string; config?: unknown }): ManifestMiddlewareEntry {
    if (typeof m === "function") {
        return { kind: "user", name: (m as Function).name };
    }
    const entry: ManifestMiddlewareEntry = { kind: "native", name: m.name };
    if (m.config !== undefined) entry.config = m.config;
    return entry;
}

function processHandlers(
    registration: ServiceRegistration,
    classNode: ts.ClassDeclaration | undefined,
    checker: ts.TypeChecker,
    registry: SchemaRegistry,
): ManifestHandler[] {
    const handlers: ManifestHandler[] = [];

    for (const [methodName, handlerDef] of registration.handlers) {
        const extracts: ManifestExtract[] = [];

        // Find the method in the AST for type information
        const methodNode = classNode
            ? findMethodDeclaration(classNode, methodName)
            : undefined;

        // The handler now takes a single input object. Look it up once so we
        // can walk individual field types out of it for body schemas. Prefer
        // the explicit type annotation (handles destructure patterns where
        // `getTypeAtLocation` would otherwise return a synthesized binding type).
        const inputParam = methodNode?.parameters[0];
        const inputType = inputParam?.type
            ? checker.getTypeFromTypeNode(inputParam.type)
            : (inputParam ? checker.getTypeAtLocation(inputParam) : undefined);

        // Process each extract descriptor (keyed by the input-object field).
        for (const [field, descriptor] of Object.entries(handlerDef.extract)) {
            // Context descriptors are runtime-only — the gateway has no concept of them.
            if (descriptor.source === "context") continue;

            const extract: ManifestExtract = {
                field,
                source: descriptor.source,
            };

            // Add name for named sources
            if (descriptor.source !== "body") {
                extract.name = (descriptor as { name: string }).name;
            }

            // Check for Zod override FIRST — skip type walking entirely
            if ("jsonSchema" in descriptor) {
                const zodDescriptor = descriptor as { jsonSchema: JsonSchema };
                const typeName = `${methodName}_${field}`;
                registry.registerZodOverride(typeName, zodDescriptor.jsonSchema);
                extract.schema = { $ref: `#/schemas/${typeName}` };
                extracts.push(extract);
                continue;
            }

            // For body descriptors, walk the field's declared type out of the
            // handler's input-object parameter (e.g. `{ body: ItemType }`).
            if (descriptor.source === "body" && inputType && inputParam) {
                const fieldSymbol = inputType.getProperty(field);
                if (fieldSymbol) {
                    const fieldType = checker.getTypeOfSymbolAtLocation(
                        fieldSymbol,
                        inputParam,
                    );
                    extract.schema = typeToJsonSchema(fieldType, checker, registry);
                }
            }

            extracts.push(extract);
        }

        // Extract response type
        let response: JsonSchema | undefined;
        if (methodNode) {
            const returnType = checker.getReturnTypeOfSignature(
                checker.getSignatureFromDeclaration(methodNode)!,
            );
            // Unwrap Promise<T> to get T
            const responseType = unwrapPromise(returnType, checker);
            if (responseType && !(responseType.flags & ts.TypeFlags.Void)) {
                response = typeToJsonSchema(
                    responseType,
                    checker,
                    registry,
                );
            }
        }

        const handlerEntry: ManifestHandler = {
            name: methodName,
            method: handlerDef.method,
            path: handlerDef.path,
            status: handlerDef.status,
            validate: handlerDef.validate ?? true,
            streaming: handlerDef.streaming ?? false,
            extract: extracts,
            response,
        };
        if (handlerDef.middleware?.length) {
            handlerEntry.middleware = handlerDef.middleware.map(toMiddlewareEntry);
        }
        handlers.push(handlerEntry);
    }

    return handlers;
}

function processDependencies(
    registration: ServiceRegistration,
): ManifestDependency[] {
    return registration.dependencies.map((dep, position) => {
        if (typeof dep === "function") {
            return { type: "class" as const, value: dep.name, position };
        }
        if (typeof dep === "symbol") {
            return {
                type: "symbol" as const,
                value: dep.description ?? dep.toString(),
                position,
            };
        }
        return { type: "string" as const, value: dep, position };
    });
}

function unwrapPromise(
    type: ts.Type,
    checker: ts.TypeChecker,
): ts.Type | undefined {
    // Check if it's Promise<T>
    const symbol = type.getSymbol();
    if (symbol && symbol.name === "Promise") {
        const typeArgs = checker.getTypeArguments(type as ts.TypeReference);
        if (typeArgs.length > 0) {
            return typeArgs[0];
        }
    }
    // Not a Promise — return as-is
    return type;
}

function computeHash(manifest: ManifestOutput): string {
    const content = JSON.stringify({
        ...manifest,
        hash: "",
        emitted_at: "",
    });
    return "sha256:" + createHash("sha256").update(content).digest("hex");
}

function writeManifestAtomic(outputPath: string, manifest: ManifestOutput): void {
    const content = JSON.stringify(manifest, null, 2);
    const tmpPath = outputPath + ".tmp";
    fs.writeFileSync(tmpPath, content);
    fs.renameSync(tmpPath, outputPath);
}