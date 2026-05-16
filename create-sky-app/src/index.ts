#!/usr/bin/env bun
import { mkdir } from "node:fs/promises";
import { join, resolve, dirname } from "node:path";

// ── Argument parsing ─────────────────────────────────────────────────────────

const args = process.argv.slice(2);
const projectName = args.find((a) => !a.startsWith("-"));
const skipInstall = args.includes("--no-install");

if (!projectName || args.includes("--help") || args.includes("-h")) {
  console.log("Usage: bunx create-sky-app <project-name> [--no-install]");
  process.exit(projectName ? 0 : 1);
}

if (!/^[a-z0-9][a-z0-9._-]*$/.test(projectName)) {
  console.error(`Error: "${projectName}" is not a valid package name`);
  process.exit(1);
}

const targetDir = resolve(process.cwd(), projectName);

if (await Bun.file(targetDir).exists()) {
  console.error(`Error: directory "${projectName}" already exists`);
  process.exit(1);
}

// ── File writing helper ──────────────────────────────────────────────────────

async function write(rel: string, content: string) {
  const full = join(targetDir, rel);
  await mkdir(dirname(full), { recursive: true });
  await Bun.write(full, content);
  console.log(`  \x1b[32m✓\x1b[0m ${rel}`);
}

// ── Templates ────────────────────────────────────────────────────────────────

const packageJson = JSON.stringify(
  {
    name: projectName,
    version: "0.1.0",
    type: "module",
    private: true,
    scripts: {
      build: "sky build",
      dev: "sky build --watch",
    },
    dependencies: {
      "sky-framework": "latest",
    },
    devDependencies: {
      "@types/bun": "latest",
      "pino-pretty": "latest",
      typescript: "latest",
    },
  },
  null,
  2
);

const tsconfigJson = JSON.stringify(
  {
    compilerOptions: {
      lib: ["ESNext"],
      target: "ESNext",
      module: "Preserve",
      moduleDetection: "force",
      moduleResolution: "bundler",
      allowImportingTsExtensions: true,
      verbatimModuleSyntax: true,
      noEmit: true,
      strict: true,
      skipLibCheck: true,
      noUncheckedIndexedAccess: true,
      types: ["bun-types"],
    },
  },
  null,
  2
);

const skyToml = `\
version = "1"

[listen]
address = "0.0.0.0:8080"
body_limit = "1mb"

[logging]
format = "pretty"
level = "info"

[worker]
bun_path = "bun"
worker_script = "./index.ts"
worker_version = "0.1.0"
readiness_timeout = "10s"
shutdown_grace = "5s"
manifest_path = "./sky-manifest.json"

[build]
sources = ["./src/services"]
`;

const indexTs = `\
import { startServer, logger } from "sky-framework/runtime";
import { HelloService } from "./src/services/hello";

const server = await startServer({
  workerVersion: "0.1.0",
  logger,
  services: [HelloService],
});

logger.info({ workerId: server.workerId }, "worker ready");

process.on("SIGINT", () => server.close());
process.on("SIGTERM", () => server.close());
`;

const helloServiceTs = `\
import { Service, Handler, ZodBody } from "sky-framework/decorators";
import { z } from "zod";

const HelloSchema = z.object({ name: z.string() });

@Service({ lifetime: "scoped" })
export class HelloService {
  @Handler({ method: "POST", path: "/hello", extract: { body: ZodBody(HelloSchema) } })
  async hello({ body }: { body: z.infer<typeof HelloSchema> }) {
    return { message: \`Hello, \${body.name}!\` };
  }
}
`;

const gitignore = `\
node_modules/
sky-manifest.json
*.sock
dist/
`;

// ── Scaffold ─────────────────────────────────────────────────────────────────

console.log(`\nCreating Sky project in \x1b[1m${projectName}/\x1b[0m\n`);

await mkdir(join(targetDir, "src/services"), { recursive: true });

await write("package.json", packageJson + "\n");
await write("tsconfig.json", tsconfigJson + "\n");
await write("sky.toml", skyToml);
await write("index.ts", indexTs);
await write("src/services/hello.ts", helloServiceTs);
await write(".gitignore", gitignore);

// ── Install ──────────────────────────────────────────────────────────────────

if (!skipInstall) {
  console.log("\nInstalling dependencies…");
  const proc = Bun.spawn(["bun", "install"], {
    cwd: targetDir,
    stdout: "inherit",
    stderr: "inherit",
  });
  const code = await proc.exited;
  if (code !== 0) {
    console.error("\nbun install failed — run it manually inside the project.");
    process.exit(1);
  }
}

// ── Next steps ───────────────────────────────────────────────────────────────

console.log(`
\x1b[1mDone!\x1b[0m  Get started:

  cd ${projectName}
${skipInstall ? "  bun install\n" : ""}\
  bun run build           # emit sky-manifest.json

Then start the gateway (download from GitHub releases or use Docker):

  # Native binary
  ./sky-gateway --config sky.toml

  # Docker
  docker run --rm -v "$(pwd)":/app -p 8080:8080 ghcr.io/sky-framework/sky:latest

Test your service:
  curl -X POST http://localhost:8080/hello \\
    -H "Content-Type: application/json" \\
    -d '{"name": "World"}'
`);
