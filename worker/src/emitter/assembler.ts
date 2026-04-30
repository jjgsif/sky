export interface ManifestOutput {
    version: "1";
    hash: string;
    emitted_at: string;
    services: ManifestService[];
    middleware: ManifestMiddleware[];
    schemas: Record<string, JsonSchema>;
}

export interface ManifestService {
    name: string;
    className: string;
    lifetime: "singleton" | "scoped" | "transient";
    dependencies: ManifestDependency[];
    handlers: ManifestHandler[];
    group?: ManifestGroup;
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
    extract: ManifestExtract[];
    response?: JsonSchema
}

export interface ManifestExtract {
    source: "body" | "query" | "param" | "header";
    name?: string;
    position: number;
    schema?: JsonSchema
}

export interface ManifestMiddleware {
    name: string;
    className: string;
    global: boolean;
    order: number;
    kind: "user" | "native";
}

export interface ManifestGroup {
    prefix: string;
    middleware: string[];
}

import ts from "typescript";
import { createProgram, findTypeDeclarations } from "./walker";
import { typeToJsonSchema, type JsonSchema } from "./schema";
import { SchemaRegistry } from "./registry";
import { ServiceMap } from "../decorators";
import type {
    ServiceRegistration,
    HandlerDefinition,
    ExtractDescriptor,
} from "../decorators";
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
                    (m: Function) => m.name
                ),
            };
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

        // Process each extract descriptor
        handlerDef.extract.forEach((descriptor, position) => {
            const extract: ManifestExtract = {
                source: descriptor.source,
                position,
            };

            // Add name for named sources
            if (descriptor.source !== "body") {
                extract.name = (descriptor as any).name;
            }

            // Check for Zod override FIRST — skip type walking entirely
            if ("jsonSchema" in descriptor) {
                const zodDescriptor = descriptor as any;
                const typeName = methodNode
                    ? getParameterTypeName(methodNode, position, checker)
                    : `${methodName}_body`;
                registry.registerZodOverride(typeName, zodDescriptor.jsonSchema);
                extract.schema = { $ref: `#/schemas/${typeName}` };
                extracts.push(extract);
                return; // skip type walking for this parameter
            }

            // For body descriptors, extract the type schema
            if (descriptor.source === "body" && methodNode) {
                const param = methodNode.parameters[position];
                if (param && param.type) {
                    const paramType = checker.getTypeAtLocation(param);
                    const schemaRef = typeToJsonSchema(
                        paramType,
                        checker,
                        registry,
                    );
                    extract.schema = schemaRef;
                }
            }

            extracts.push(extract);
        });

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

        handlers.push({
            name: methodName,
            method: handlerDef.method,
            path: handlerDef.path,
            status: handlerDef.status,
            validate: handlerDef.validate ?? true,
            extract: extracts,
            response,
        });
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

function getParameterTypeName(
    method: ts.MethodDeclaration,
    position: number,
    checker: ts.TypeChecker,
): string {
    const param = method.parameters[position];
    if (param && param.type) {
        const type = checker.getTypeAtLocation(param);
        const symbol = type.getSymbol();
        if (symbol) return symbol.name;
    }
    return `param_${position}`;
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