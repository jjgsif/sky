---
description: Scaffold a new Sky @Service (handler-bearing or injectable-only)
argument-hint: [ServiceName] [--handler|--injectable] [--lifetime=singleton|scoped|transient]
---

# /new-service

Scaffold a new Sky `@Service` class — either a **Handler service** (HTTP-facing,
decorated methods become routes) or an **Injectable service** (DI-only, used as
a constructor dependency of other services).

Arguments passed by the user: `$ARGUMENTS`

## Step 1 — Gather inputs

Parse `$ARGUMENTS` if present. Otherwise (or for any missing field) ask the
user with a single concise question per turn. Required fields:

1. **Name** — PascalCase class name. If the user passes `users` or `user`,
   normalize to `UserService` (capitalize, append `Service` if absent).
2. **Kind** — `handler` (default) or `injectable`.
   - `handler` → service exposes one or more `@Handler` methods, lives in
     `worker/src/runtime/services/`, gets registered with `startServer({ services: [...] })`.
   - `injectable` → no `@Handler` methods; meant to be injected into other
     services via constructor DI. Lives in `worker/src/runtime/services/` too,
     but only needs registration if it is referenced as a dependency from
     another registered service.
3. **Lifetime** — `singleton` | `scoped` | `transient`. Default `scoped`
   unless the user specified otherwise. Recap the trade-off briefly only if
   the user seems unsure:
   - `singleton`: shared across the worker's lifetime. Good for stateless
     utilities, caches, connection pools.
   - `scoped` (default): one instance per request. Good for services that
     hold per-request state (auth context, transaction).
   - `transient`: new instance every resolution. Rare.
4. **For handler kind only** — at least one initial route:
   - HTTP method (GET / POST / PUT / PATCH / DELETE)
   - Path (e.g. `/users/:id`, `/orders`)
   - Optional extractors: any of `Body()`, `ZodBody(schema)`, `Param("id")`,
     `Query("q")`, `Header("authorization")`. Ask only if the path has
     params or the method is POST/PUT/PATCH (those usually want a body).

Do **not** invent fields the user didn't ask for. If the user gives the bare
minimum, generate the bare minimum.

## Step 2 — Pick the file path

- Handler service: `worker/src/runtime/services/<kebab-name>.ts`
- Injectable service: `worker/src/runtime/services/<kebab-name>.ts`

`<kebab-name>` is the class name without the `Service` suffix, kebab-cased.
`UserService` → `user.ts`. `OrderHistoryService` → `order-history.ts`.

If the file already exists, **stop and report**. Do not overwrite.

## Step 3 — Generate the file

### Handler service template

```ts
import { Service, Handler /*, Body, Param, Query, Header, ZodBody */ } from "@sky/decorators";

@Service({ lifetime: "<lifetime>" })
class <ServiceName> {
  @Handler({
    method: "<METHOD>",
    path: "<path>",
    extract: [/* extractors here */],
  })
  async <handlerName>(/* params matching extract order */) {
    // TODO: implement
    return {};
  }
}

export { <ServiceName> };
```

Pattern to follow exactly: see `worker/src/runtime/services/hello.ts`. Match
its import style (`@sky/decorators`), its decoration order, and its
single-export-at-bottom style.

### Injectable service template

```ts
import { Service } from "@sky/decorators";

@Service({ lifetime: "<lifetime>" })
class <ServiceName> {
  // TODO: methods used by the services that inject this one.
}

export { <ServiceName> };
```

## Step 4 — Wire it into the worker entry

For both kinds, edit `worker/src/index.ts`:

1. Add an `import { <ServiceName> } from "./runtime/services/<kebab-name>";`
   alongside the existing service imports.
2. Add `<ServiceName>` to the `services: [...]` array passed to `startServer`.

Skip wiring the injectable into the `services: [...]` array **only if** the
user explicitly says it'll be registered transitively. Otherwise add it —
unregistered injectables silently fail to resolve.

## Step 5 — Regenerate the manifest

Run from the workspace root (the directory containing `sky.toml`):

```bash
bun run worker/src/cli.ts build
```

If the user is in a different directory, adjust accordingly. Report the
service / handler / schema counts that the CLI prints.

## Step 6 — Confirm

End with a one-line summary: file path, kind, lifetime, and (for handler
services) the route line. Don't write a long recap — the diff already shows
the work.

## Guardrails

- **Don't add example/sample logic** the user didn't request — leave a
  `// TODO: implement` and stop.
- **Don't add tests** unless asked.
- **Don't add comments explaining what the decorators do** — the project
  already documents them in `worker/Decorators.md`.
- **Don't commit** — the user controls that.
- If `worker/sky-manifest.json` and the workspace-root `sky-manifest.json`
  are both present, regenerate whichever matches the user's current `cwd`
  (look at where they last ran `sky build` from). When in doubt, ask.
