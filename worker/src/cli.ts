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
import type { Subprocess } from "bun";

// ── Types ───────────────────────────────────────────────

interface SkyConfig {
  version: string;
  manifest_path?: string;
  worker?: Record<string, unknown>;
  build?: {
    sources?: string[];
  };
  frontend?: {
    build?: string;
    output?: string;
    dev_server?: string;
    dev_command?: string;
    prefix?: string;
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
  const manifestPath = config.manifest_path ?? DEFAULT_MANIFEST_PATH;
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

// ── Dev mode ────────────────────────────────────────────

const GATEWAY_CANDIDATES = [
  "sky-gateway",
  "./target/debug/sky-gateway",
  "./target/release/sky-gateway",
] as const;

function resolveGateway(override?: string): string {
  if (override) {
    if (!existsSync(override)) {
      fatal(`gateway binary not found at: ${override}`);
    }
    return override;
  }
  // Try each candidate: shell PATH lookup first, then local cargo outputs.
  for (const candidate of GATEWAY_CANDIDATES) {
    if (candidate.startsWith(".")) {
      if (existsSync(candidate)) return candidate;
    } else {
      // Bare name — rely on PATH via Bun.which.
      if (Bun.which(candidate)) return candidate;
    }
  }
  fatal(
    "sky-gateway binary not found.\n" +
      "  • Add it to PATH, or\n" +
      "  • Run `cargo build -p sky-gateway` to build locally."
  );
}

async function devMode(
  rootDir: string,
  config: SkyConfig,
  configPath: string,
  gatewayOverride?: string
): Promise<void> {
  const gatewayBin = resolveGateway(gatewayOverride);

  info(`starting gateway: ${gatewayBin} --dev`);

  const gatewayProc = Bun.spawn(
    [gatewayBin, "--config", configPath, "--dev"],
    {
      cwd: rootDir,
      stdout: "pipe",
      stderr: "pipe",
    }
  );

  // Pipe gateway output with a label prefix so it's identifiable in the terminal.
  pipeWithPrefix(gatewayProc.stdout as ReadableStream<Uint8Array>, "\x1b[32m[gateway]\x1b[0m");
  pipeWithPrefix(gatewayProc.stderr as ReadableStream<Uint8Array>, "\x1b[32m[gateway]\x1b[0m");

  let frontendProc: Subprocess | null = null;

  if (config.frontend?.dev_command) {
    const devCmd = config.frontend.dev_command;
    info(`starting frontend: ${devCmd}`);
    frontendProc = Bun.spawn(["sh", "-c", devCmd], {
      cwd: rootDir,
      stdout: "pipe",
      stderr: "pipe",
    });
    pipeWithPrefix(frontendProc.stdout as ReadableStream<Uint8Array>, "\x1b[35m[frontend]\x1b[0m");
    pipeWithPrefix(frontendProc.stderr as ReadableStream<Uint8Array>, "\x1b[35m[frontend]\x1b[0m");
  } else if (config.frontend?.dev_server) {
    info(`proxying frontend to ${config.frontend.dev_server} (no dev_command configured)`);
  }

  // Handle Ctrl-C / SIGTERM: shut down all children gracefully.
  const shutdown = () => {
    info("shutting down...");
    try { gatewayProc.kill("SIGTERM"); } catch {}
    if (frontendProc) try { frontendProc.kill("SIGTERM"); } catch {}
    setTimeout(() => {
      try { gatewayProc.kill("SIGKILL"); } catch {}
      if (frontendProc) try { frontendProc.kill("SIGKILL"); } catch {}
      process.exit(0);
    }, 5000).unref();
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);

  // Supervise: gateway crash is fatal; frontend crash is a warning.
  gatewayProc.exited.then((code) => {
    error(`[gateway] exited with code ${code}`);
    if (frontendProc) try { frontendProc.kill("SIGTERM"); } catch {}
    process.exit(1);
  });

  if (frontendProc) {
    frontendProc.exited.then((code) => {
      warn(
        `[frontend] dev server exited with code ${code}. ` +
          `Gateway is still running. Restart the frontend manually or check dev_command in sky.toml.`
      );
    });
  }

  // Keep the process alive until a signal or gateway exit terminates it.
  await new Promise<never>(() => {});
}

async function pipeWithPrefix(
  stream: ReadableStream<Uint8Array>,
  prefix: string
): Promise<void> {
  const decoder = new TextDecoder();
  const reader = stream.getReader();
  let partial = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const text = partial + decoder.decode(value, { stream: true });
      const lines = text.split("\n");
      partial = lines.pop() ?? "";
      for (const line of lines) {
        if (line.trim()) console.log(`${prefix} ${line}`);
      }
    }
    if (partial.trim()) console.log(`${prefix} ${partial}`);
  } finally {
    reader.releaseLock();
  }
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
      watch:   { type: "boolean", short: "w", default: false },
      config:  { type: "string",  short: "c" },
      gateway: { type: "string" },
      help:    { type: "boolean", short: "h", default: false },
    },
    allowPositionals: true,
    strict: false,
  });

  if (values.help || positionals.length === 0) {
    console.log(`
Usage: sky <command> [options]

Commands:
  build    Scan @Service classes, emit sky-manifest.json
  dev      Start gateway in dev mode and optionally spawn the frontend dev server

Options:
  -w, --watch        Watch for changes and rebuild (build only)
  -c, --config       Path to sky.toml (default: ./sky.toml)
      --gateway      Path to sky-gateway binary (dev only; auto-detected by default)
  -h, --help         Show this help

Environment:
  SKY_DEBUG=1    Enable verbose stack traces on error
`);
    process.exit(values.help ? 0 : 1);
  }

  const command = positionals[0];

  const rootDir = values.config
    ? dirname(resolve(values.config as string))
    : process.cwd();

  const configPath = values.config
    ? resolve(values.config as string)
    : join(rootDir, "sky.toml");

  const config = await loadConfig(rootDir);

  if (command === "build") {
    const ctx = buildContext(rootDir, config);
    if (values.watch) {
      await watchMode(ctx);
    } else {
      const ok = await build(ctx);
      process.exit(ok ? 0 : 1);
    }
  } else if (command === "dev") {
    await devMode(rootDir, config, configPath, values.gateway as string | undefined);
  } else {
    fatal(`unknown command: '${command}'. Run 'sky --help' for usage.`);
  }
}

main().catch((err) => {
  error(`fatal: ${err.message}`);
  process.exit(1);
});
