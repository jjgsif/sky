# create-sky-app

Scaffold a new [Sky framework](https://github.com/sky-framework/sky) project in one command.

```bash
bunx create-sky-app my-app
```

## Usage

```
bunx create-sky-app <project-name> [--no-install]
```

| Argument | Description |
|---|---|
| `<project-name>` | Name for the new project directory. Must match `[a-z0-9][a-z0-9._-]*` (npm package naming rules). |
| `--no-install` | Skip running `bun install` after scaffolding. |
| `-h`, `--help` | Print usage and exit. |

## What gets generated

```
<project-name>/
├── package.json          # sky-framework dependency, build/dev scripts
├── tsconfig.json         # strict ESNext, Bun types
├── sky.toml              # gateway config (listen, worker, build sources)
├── index.ts              # worker entry point — imports services, calls startServer()
├── src/
│   └── services/
│       └── hello.ts      # example @Service with a POST /hello handler
└── .gitignore
```

### Generated `sky.toml`

```toml
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
```

### Generated `src/services/hello.ts`

```ts
import { Service, Handler, ZodBody } from "sky-framework/decorators";
import { z } from "zod";

const HelloSchema = z.object({ name: z.string() });

@Service({ lifetime: "scoped" })
export class HelloService {
  @Handler({ method: "POST", path: "/hello", extract: { body: ZodBody(HelloSchema) } })
  async hello({ body }: { body: z.infer<typeof HelloSchema> }) {
    return { message: `Hello, ${body.name}!` };
  }
}
```

## Next steps

```bash
cd my-app
bun run build          # emit sky-manifest.json

# Download the gateway binary from GitHub Releases and run it:
./sky-gateway --config sky.toml

# Or use Docker:
docker run --rm -v "$(pwd)":/app -p 8080:8080 ghcr.io/sky-framework/sky:latest
```

Test your service:

```bash
curl -X POST http://localhost:8080/hello \
  -H "Content-Type: application/json" \
  -d '{"name": "World"}'
# → {"message":"Hello, World!"}
```

## Local development (monorepo)

If you are working inside the Sky monorepo itself, run the scaffolder directly:

```bash
bun run src/index.ts my-app
```
