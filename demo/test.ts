/**
 * Demo Application — Integration Test
 *
 * Exercises every endpoint in the demo application via the
 * Sky gateway HTTP API.
 *
 * Prerequisites:
 *   1. Start the worker:   SKY_WORKER_ID=1 bun run index.ts
 *   2. Start the gateway:  cargo run -p sky-gateway -- --config ./sky.toml
 *   3. Run tests:          bun run test.ts
 */

const BASE = "http://127.0.0.1:8080";

// ── Helpers ─────────────────────────────────────────────

let passed = 0;
let failed = 0;

async function get(
  path: string,
  headers: Record<string, string> = {},
): Promise<{ status: number; body: any }> {
  const res: Response = await fetch(`${BASE}${path}`, { headers });
  return { status: res.status, body: await json(res) };
}

async function post(
  path: string,
  payload: Record<string, any> = {},
): Promise<{ status: number; body: any }> {
  const res: globalThis.Response = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  return { status: res.status, body: await json(res) };
}

async function put(
  path: string,
  payload: Record<string, any> = {},
): Promise<{ status: number; body: any }> {
  const res: globalThis.Response = await fetch(`${BASE}${path}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  return { status: res.status, body: await json(res) };
}

async function del(path: string): Promise<{ status: number; body: any }> {
  const res: Response = await fetch(`${BASE}${path}`, { method: "DELETE" });
  return { status: res.status, body: await json(res) };
}

async function json(response: Response): Promise<any> {
  const text = await response.text();
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

async function test(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
    passed++;
    console.log(`  ✓ ${name}`);
  } catch (err: any) {
    failed++;
    console.log(`  ✗ ${name}`);
    console.log(`    ${err.message}`);
  }
}

function assert(cond: boolean, msg: string): void {
  if (!cond) throw new Error(msg);
}

function eq(actual: any, expected: any, label: string): void {
  assert(
    actual === expected,
    `${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
  );
}

// ── Tests ────────────────────────────────────────────────

async function run() {
  console.log("\nSky Demo — Integration Tests\n");

  // ── Health ──────────────────────────────────────────────

  console.log("HealthService:");

  await test("GET /health returns healthy status", async () => {
    const { status, body } = await get("/health");
    eq(status, 200, "HTTP status");
    eq(body.status, "healthy", "health status");
    assert(body.startedAt !== undefined, "should have startedAt");
    assert(body.uptime !== undefined, "should have uptime");
    console.log(`    uptime: ${body.uptime}`);
  });

  // ── Users CRUD ──────────────────────────────────────────

  console.log("\nUserService:");

  let userId: string;

  await test("POST /users creates a user with Zod validation", async () => {
    const { status, body } = await post("/users", {
      name: "Alice",
      email: "alice@example.com",
      role: "admin",
    });
    eq(status, 201, "HTTP status");
    assert(body.id !== undefined, "should have id");
    eq(body.name, "Alice", "name");
    eq(body.email, "alice@example.com", "email");
    eq(body.role, "admin", "role");
    userId = body.id;
    console.log(`    created user ${userId}`);
  });

  await test("POST /users rejects invalid email", async () => {
    const { status, body } = await post("/users", {
      name: "Bob",
      email: "not-an-email",
    });
    assert(status >= 400, `expected error status, got ${status}`);
    console.log(`    response: ${JSON.stringify(body)}`);
  });

  await test("POST /users rejects missing name", async () => {
    const { status, body } = await post("/users", {
      email: "bob@example.com",
    });
    assert(status >= 400, `expected error status, got ${status}`);
    console.log(`    response: ${JSON.stringify(body)}`);
  });

  await test("GET /users/:id returns the created user", async () => {
    const { status, body } = await get(`/users/${userId}`);
    eq(status, 200, "HTTP status");
    eq(body.id, userId, "id");
    eq(body.name, "Alice", "name");
  });

  await test("GET /users/:id returns 404 for missing user", async () => {
    const { status } = await get("/users/999");
    eq(status, 404, "HTTP status");
  });

  await test("POST /users creates a second user", async () => {
    const { status, body } = await post("/users", {
      name: "Bob",
      email: "bob@example.com",
    });
    eq(status, 201, "HTTP status");
    eq(body.name, "Bob", "name");
    eq(body.role, "member", "default role");
    console.log(`    created user ${body.id} with default role`);
  });

  await test("GET /users lists users with pagination", async () => {
    const { status, body } = await get("/users?page=1&limit=10");
    eq(status, 200, "HTTP status");
    assert(body.items.length >= 2, "should have at least 2 users");
    eq(body.page, 1, "page");
    console.log(`    total: ${body.total}, showing: ${body.items.length}`);
  });

  await test("PUT /users/:id updates a user", async () => {
    const { status, body } = await put(`/users/${userId}`, {
      name: "Alice Updated",
    });
    eq(status, 200, "HTTP status");
    eq(body.name, "Alice Updated", "updated name");
    eq(body.email, "alice@example.com", "email unchanged");
  });

  await test("DELETE /users/:id removes a user", async () => {
    const { status } = await del(`/users/${userId}`);
    eq(status, 204, "HTTP status");
  });

  await test("GET /users/:id returns 404 after deletion", async () => {
    const { status } = await get(`/users/${userId}`);
    eq(status, 404, "HTTP status");
  });

  // ── Admin ───────────────────────────────────────────────

  console.log("\nAdminService:");

  await test("GET /api/admin/users with valid token", async () => {
    const { status, body } = await get("/api/admin/users?page=1", {
      "x-admin-token": "sky-admin-secret",
    });
    eq(status, 200, "HTTP status");
    eq(body.admin, true, "admin flag");
    console.log(`    response: ${JSON.stringify(body)}`);
  });

  await test("GET /api/admin/users with invalid token returns 401", async () => {
    const { status } = await get("/api/admin/users?page=1", {
      "x-admin-token": "wrong-token",
    });
    eq(status, 401, "HTTP status");
  });

  await test("GET /api/admin/stats with valid token", async () => {
    const { status, body } = await get("/api/admin/stats", {
      "x-admin-token": "sky-admin-secret",
    });
    eq(status, 200, "HTTP status");
    assert(body.totalUsers !== undefined, "should have totalUsers");
    console.log(`    stats: ${JSON.stringify(body)}`);
  });

  // ── Summary ─────────────────────────────────────────────

  console.log("\n─────────────────────────────────────────");

  if (failed > 0) {
    console.log(`\n${passed} passed, ${failed} failed\n`);
    process.exit(1);
  }

  console.log(`\nAll ${passed} tests passed\n`);
}

run().catch((err) => {
  console.error("\nFatal:", err.message);
  console.error("\nIs the gateway running?");
  console.error("  1. SKY_WORKER_ID=1 bun run index.ts");
  console.error("  2. cargo run -p sky-gateway -- --config ./sky.toml\n");
  process.exit(1);
});
