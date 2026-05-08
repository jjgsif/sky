import ts from "typescript";

/**
 * Create a TypeScript Program from a set of source files.
 *
 * The Program is the entry point to the Compiler API. It parses
 * the files, resolves imports, and provides access to the type
 * checker. We configure it with strict mode to match the user's
 * likely tsconfig.
 */
export function createProgram(filePaths: string[]): ts.Program {
    // Pull baseUrl + paths from the nearest tsconfig.json so path aliases like
    // `@sky/decorators` resolve correctly during type walking. Without this,
    // imports become `any` and mapped types like `ExtractContext` collapse.
    const configPath = ts.findConfigFile(
        filePaths[0] ?? process.cwd(),
        ts.sys.fileExists,
        "tsconfig.json",
    );

    let parsedOptions: ts.CompilerOptions = {};
    if (configPath) {
        const configFile = ts.readConfigFile(configPath, ts.sys.readFile);
        const parsed = ts.parseJsonConfigFileContent(
            configFile.config,
            ts.sys,
            configPath.replace(/[\\/][^\\/]+$/, ""),
        );
        parsedOptions = parsed.options;
    }

    return ts.createProgram(filePaths, {
        ...parsedOptions,
        target: ts.ScriptTarget.ESNext,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
        strict: true,
        noEmit: true,
    });
}

/**
 * Find all interface and type alias declarations in a source file.
 *
 * This is a shallow walk — it finds top-level declarations only,
 * not interfaces nested inside functions or classes. For Sky's
 * purposes, DTOs are always top-level, so this is sufficient.
 */
export interface TypeDeclaration {
    name: string;
    type: ts.Type;
    node: ts.Node;
}

export function findTypeDeclarations(
    sourceFile: ts.SourceFile,
    checker: ts.TypeChecker,
): TypeDeclaration[] {
    const declarations: TypeDeclaration[] = [];

    ts.forEachChild(sourceFile, (node) => {
        // Interface: interface Foo { ... }
        if (ts.isInterfaceDeclaration(node)) {
            const type = checker.getTypeAtLocation(node);
            declarations.push({
                name: node.name.text,
                type,
                node,
            });
        }

        // Type alias: type Foo = { ... }
        if (ts.isTypeAliasDeclaration(node)) {
            const type = checker.getTypeAtLocation(node);
            declarations.push({
                name: node.name.text,
                type,
                node,
            });
        }
    });

    return declarations;
}