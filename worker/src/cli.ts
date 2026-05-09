#!/usr/bin/env bun

/**
 * Sky CLI
 *
 * Build tool for the Sky framework. Reads sky.toml, imports service
 * modules to trigger decorators, runs the type walker and manifest
 * assembler, and writes sky-manifest.json.
 *
 * Commands:
 *   sky build           — one-shot build
 *   sky build --watch   — watch mode with automatic rebuild
 */

import { parseArgs } from "util";
import { existsSync, watch as fsWatch } from "fs";
import { resolve, join, dirname } from "path";
import { assembleManifest } from "./emitter/assembler";

// ── Types ───────────────────────────────────────────────

interface SkyConfig {
  version: string;
  worker?: {
    manifest_path?: string;
  };
  build?: {
    sources?: string[];
  };
}

interface BuildContext {
  /** Resolved source entry points */
  entryPoints: string[];
  /** Configured source globs (kept for watch mode re-resolution) */
  sources: string[];
  /** Output path for sky-manifest.json */
  manifestPath: string;
  /** Root directory (where sky.toml lives) */
  rootDir: string;
}

// ── Config ──────────────────────────────────────────────

const DEFAULT_SOURCES = ["./src/services"];
const DEFAULT_MANIFEST_PATH = "./sky-manifest.json";

async function loadConfig(rootDir: string): Promise<SkyConfig> {
  const configPath = join(rootDir, "sky.toml");

  if (!existsSync(configPath)) {
    fatal(`sky.toml not found in ${rootDir}`);
  }

  try {
    return Bun.TOML.parse(await Bun.file(configPath).text()) as unknown as SkyConfig;
  } catch (err: any) {
    fatal(`Failed to parse sky.toml: ${err.message}`);
  }
}

function buildContext(rootDir: string, config: SkyConfig): BuildContext {
  const sources = config.build?.sources ?? DEFAULT_SOURCES;
  const manifestPath = config.worker?.manifest_path ?? DEFAULT_MANIFEST_PATH;
  const entryPoints = resolveEntryPoints(rootDir, sources);

  if (entryPoints.length === 0) {
    fatal(`No source files found in: ${sources.join(", ")}`);
  }

  return {
    entryPoints,
    sources,
    manifestPath: resolve(rootDir, manifestPath),
    rootDir,
  };
}

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
  info(`scanning ${ctx.entryPoints.length} source file(s)...`);

  try {
    const manifest = await assembleManifest(ctx.entryPoints, ctx.manifestPath);
    const elapsed = (performance.now() - startTime).toFixed(0);

    const handlerCount = manifest.services.reduce(
      (n, s) => n + s.handlers.length, 0
    );

    info(
      `${manifest.services.length} service(s), ${handlerCount} handler(s), ` +
      `${Object.keys(manifest.schemas).length} schema(s) — ${elapsed}ms`
    );
    info(`manifest → ${ctx.manifestPath}`);
    return true;
  } catch (err: any) {
    error(`build failed: ${err.message}`);
    if (err.stack) debug(err.stack);
    return false;
  }
}

// ── Watch mode ──────────────────────────────────────────

async function watchMode(ctx: BuildContext): Promise<void> {
  info("watching for changes (Ctrl-C to stop)...");

  await build(ctx);

  let rebuildTimer: ReturnType<typeof setTimeout> | null = null;
  const DEBOUNCE_MS = 200;

  const watchDirs = new Set(
    ctx.entryPoints.map((f) => dirname(f))
  );

  for (const dir of watchDirs) {
    fsWatch(dir, { recursive: true }, (_eventType, filename) => {
      if (!filename?.endsWith(".ts") || filename.endsWith(".d.ts")) return;

      if (rebuildTimer) clearTimeout(rebuildTimer);

      rebuildTimer = setTimeout(async () => {
        info(`change detected: ${filename}`);
        const freshCtx = {
          ...ctx,
          entryPoints: resolveEntryPoints(ctx.rootDir, ctx.sources),
        };
        await build(freshCtx);
        info("watching for changes...");
      }, DEBOUNCE_MS);
    });
  }

  await new Promise(() => {});
}

// ── Logging ─────────────────────────────────────────────

function info(msg: string): void {
  console.log(`\x1b[36m[sky]\x1b[0m ${msg}`);
}

function warn(msg: string): void {
  console.warn(`\x1b[33m[sky]\x1b[0m ${msg}`);
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

// ── Entry point ─────────────────────────────────────────

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

  if (values.help || positionals.length === 0) {
    console.log(`
Usage: sky <command> [options]

Commands:
  build    Scan @Service classes, emit sky-manifest.json

Options:
  -w, --watch    Watch for changes and rebuild automatically
  -c, --config   Path to sky.toml (default: ./sky.toml)
  -h, --help     Show this help

Environment:
  SKY_DEBUG=1    Enable verbose stack traces on error
`);
    process.exit(values.help ? 0 : 1);
  }

  const command = positionals[0];

  if (command !== "build") {
    fatal(`unknown command: '${command}'. Run 'sky --help' for usage.`);
  }

  const rootDir = values.config
    ? dirname(resolve(values.config as string))
    : process.cwd();

  const config = await loadConfig(rootDir);
  const ctx = buildContext(rootDir, config);

  if (values.watch) {
    await watchMode(ctx);
  } else {
    const ok = await build(ctx);
    process.exit(ok ? 0 : 1);
  }
}

main().catch((err) => {
  error(`fatal: ${err.message}`);
  process.exit(1);
});
