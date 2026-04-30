#!/usr/bin/env bun

/**
 * Sky CLI
 *
 * Build tool for the Sky framework. Reads sky.toml, runs the
 * manifest assembler and proto emitter, generates buf config
 * if needed, and compiles protos to TypeScript.
 *
 * Commands:
 *   sky build           — one-shot build
 *   sky build --watch   — watch mode with automatic rebuild
 */

import { parseArgs } from "util";
import { readFileSync, existsSync, writeFileSync, watch as fsWatch } from "fs";
import { resolve, join, dirname } from "path";
import { assembleManifest } from "./emitter/assembler";
import { ProtoEmitter } from "./emitter/proto-emitter";

// ── Types ───────────────────────────────────────────────

interface SkyConfig {
  version: string;
  listen?: {
    address?: string;
    body_limit?: string;
  };
  worker?: {
    manifest_path?: string;
    worker_script?: string;
  };
  build?: {
    sources?: string[];
    proto_out?: string;
    buf_config?: string;
  };
}

interface BuildContext {
  /** Resolved source entry points */
  entryPoints: string[];
  /** Output path for sky-manifest.json */
  manifestPath: string;
  /** Output directory for generated .proto files */
  protoOutDir: string;
  /** Path to buf.gen.yaml */
  bufConfigPath: string;
  /** Root directory (where sky.toml lives) */
  rootDir: string;
}

// ── Config ──────────────────────────────────────────────

const DEFAULT_SOURCES = ["./src/services"];
const DEFAULT_MANIFEST_PATH = "./sky-manifest.json";
const DEFAULT_PROTO_OUT = "./proto";
const DEFAULT_BUF_CONFIG = "./buf.gen.yaml";

function loadConfig(rootDir: string): SkyConfig {
  const configPath = join(rootDir, "sky.toml");

  if (!existsSync(configPath)) {
    fatal(`sky.toml not found in ${rootDir}`);
  }

  const raw = readFileSync(configPath, "utf-8");

  try {
    return Bun.TOML.parse(raw) as unknown as SkyConfig;
  } catch (err: any) {
    fatal(`Failed to parse sky.toml: ${err.message}`);
  }
}

function buildContext(rootDir: string, config: SkyConfig): BuildContext {
  const sources = config.build?.sources ?? DEFAULT_SOURCES;
  const manifestPath = config.worker?.manifest_path ?? DEFAULT_MANIFEST_PATH;
  const protoOutDir = config.build?.proto_out ?? DEFAULT_PROTO_OUT;
  const bufConfigPath = config.build?.buf_config ?? DEFAULT_BUF_CONFIG;

  // Resolve source globs to actual files
  const entryPoints = resolveEntryPoints(rootDir, sources);

  if (entryPoints.length === 0) {
    fatal(`No source files found in: ${sources.join(", ")}`);
  }

  return {
    entryPoints,
    manifestPath: resolve(rootDir, manifestPath),
    protoOutDir: resolve(rootDir, protoOutDir),
    bufConfigPath: resolve(rootDir, bufConfigPath),
    rootDir,
  };
}

/**
 * Resolve source directories/globs to actual .ts file paths.
 */
function resolveEntryPoints(rootDir: string, sources: string[]): string[] {
  const glob = new Bun.Glob("**/*.ts");
  const files: string[] = [];

  for (const source of sources) {
    const absDir = resolve(rootDir, source);

    if (!existsSync(absDir)) {
      warn(`Source directory not found: ${source}`);
      continue;
    }

    for (const match of glob.scanSync({ cwd: absDir, absolute: true })) {
      // Skip test files, declaration files, and index barrels
      if (
        match.endsWith(".test.ts") ||
        match.endsWith(".spec.ts") ||
        match.endsWith(".d.ts")
      ) {
        continue;
      }

      files.push(match);
    }
  }

  return files;
}

// ── Build pipeline ──────────────────────────────────────

async function build(ctx: BuildContext): Promise<boolean> {
  const startTime = performance.now();

  info("starting build");

  // Step 1: Assemble manifest (imports modules, runs type walker,
  // produces JSON Schemas, writes sky-manifest.json)
  let manifest;
  try {
    info(`scanning ${ctx.entryPoints.length} source file(s)`);
    manifest = await assembleManifest(ctx.entryPoints, ctx.manifestPath);
    info(`manifest written to ${ctx.manifestPath}`);
    info(
      `  ${manifest.services.length} service(s), ` +
      `${Object.keys(manifest.schemas).length} schema(s)`
    );
  } catch (err: any) {
    error(`manifest assembly failed: ${err.message}`);
    if (err.stack) debug(err.stack);
    return false;
  }

  // Step 2: Generate .proto files from manifest
  try {
    const protoEmitter = new ProtoEmitter(manifest, {
      outDir: ctx.protoOutDir,
    });
    const protoFiles = await protoEmitter.emit();
    info(`generated ${protoFiles.length} proto file(s) in ${ctx.protoOutDir}`);
  } catch (err: any) {
    error(`proto emission failed: ${err.message}`);
    if (err.stack) debug(err.stack);
    return false;
  }

  // Step 3: Ensure buf.gen.yaml exists
  ensureBufConfig(ctx);

  // Step 4: Run buf generate
  try {
    await runBufGenerate(ctx);
    info("proto compilation complete");
  } catch (err: any) {
    error(`buf generate failed: ${err.message}`);
    return false;
  }

  const elapsed = (performance.now() - startTime).toFixed(0);
  info(`build complete in ${elapsed}ms`);

  return true;
}

// ── buf integration ─────────────────────────────────────

const DEFAULT_BUF_GEN_YAML = `# Generated by sky build. Customize as needed.
version: v2
plugins:
  - local: protoc-gen-es
    out: ./src/gen
    opt:
      - target=ts

inputs:
  - directory: ./proto/generated
`;

function ensureBufConfig(ctx: BuildContext): void {
  if (existsSync(ctx.bufConfigPath)) {
    debug(`buf config exists at ${ctx.bufConfigPath}`);
    return;
  }

  info(`generating ${ctx.bufConfigPath}`);
  writeFileSync(ctx.bufConfigPath, DEFAULT_BUF_GEN_YAML, "utf-8");
}

async function runBufGenerate(ctx: BuildContext): Promise<void> {
  const proc = Bun.spawn(["buf", "generate"], {
    cwd: ctx.rootDir,
    stdout: "pipe",
    stderr: "pipe",
  });

  const exitCode = await proc.exited;

  if (exitCode !== 0) {
    const stderr = await new Response(proc.stderr).text();
    throw new Error(`buf generate exited with code ${exitCode}\n${stderr}`);
  }
}

// ── Watch mode ──────────────────────────────────────────

async function watchMode(ctx: BuildContext): Promise<void> {
  info("watching for changes...");

  // Initial build
  await build(ctx);

  // Debounce rebuilds
  let rebuildTimer: ReturnType<typeof setTimeout> | null = null;
  const DEBOUNCE_MS = 200;

  const sourceDirs = new Set(
    ctx.entryPoints.map((f) => dirname(f))
  );

  for (const dir of sourceDirs) {
    fsWatch(dir, { recursive: true }, (eventType, filename) => {
      if (!filename || !filename.endsWith(".ts")) return;
      if (filename.endsWith(".d.ts")) return;

      if (rebuildTimer) clearTimeout(rebuildTimer);

      rebuildTimer = setTimeout(async () => {
        info(`change detected: ${filename}`);
        // Re-resolve entry points in case files were added/removed
        const freshCtx = {
          ...ctx,
          entryPoints: resolveEntryPoints(
            ctx.rootDir,
            [DEFAULT_SOURCES].flat()
          ),
        };
        await build(freshCtx);
        info("watching for changes...");
      }, DEBOUNCE_MS);
    });
  }

  // Keep the process alive
  await new Promise(() => {});
}

// ── Logging ─────────────────────────────────────────────

function info(msg: string): void {
  console.log(`\x1b[36m[sky]\x1b[0m ${msg}`);
}

function warn(msg: string): void {
  console.log(`\x1b[33m[sky]\x1b[0m ${msg}`);
}

function error(msg: string): void {
  console.error(`\x1b[31m[sky]\x1b[0m ${msg}`);
}

function debug(msg: string): void {
  if (process.env.SKY_DEBUG) {
    console.log(`\x1b[90m[sky]\x1b[0m ${msg}`);
  }
}

function fatal(msg: string): never {
  error(msg);
  process.exit(1);
}

// ── CLI entry point ─────────────────────────────────────

async function main() {
  const { values, positionals } = parseArgs({
    args: Bun.argv.slice(2),
    options: {
      watch: { type: "boolean", short: "w", default: false },
      config: { type: "string", short: "c" },
      help: { type: "boolean", short: "h", default: false },
    },
    allowPositionals: true,
    strict: false,
  });

  if (values.help) {
    console.log(`
Usage: sky <command> [options]

Commands:
  build          Build the manifest and generate proto files

Options:
  -w, --watch    Watch for changes and rebuild automatically
  -c, --config   Path to sky.toml (default: ./sky.toml)
  -h, --help     Show this help message

Environment:
  SKY_DEBUG=1    Enable debug logging
`);
    process.exit(0);
  }

  const command = positionals[0];

  if (!command) {
    fatal("no command specified. Run 'sky --help' for usage.");
  }

  if (command !== "build") {
    fatal(`unknown command: ${command}. Run 'sky --help' for usage.`);
  }

  // Resolve root directory
  const rootDir = values.config
    ? dirname(resolve(values.config as string))
    : process.cwd();

  const config = loadConfig(rootDir);
  const ctx = buildContext(rootDir, config);

  if (values.watch) {
    await watchMode(ctx);
  } else {
    const success = await build(ctx);
    process.exit(success ? 0 : 1);
  }
}

main().catch((err) => {
  error(`fatal: ${err.message}`);
  process.exit(1);
});
