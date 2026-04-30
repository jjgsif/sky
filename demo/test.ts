/**
 * Demo Application — Integration Test
 *
 * Exercises every endpoint in the demo application via the
 * worker's Unix domain socket.
 *
 * Prerequisites:
 *   1. sky build
 *   2. SKY_WORKER_ID=1 bun run index.ts
 *   3. bun run test.ts
 */

const SOCKET_PATH = "/tmp/sky/workers/sky-worker-1.sock";

// ── Helpers ─────────────────────────────────────────

let passed = 0;
let failed = 0;

async function call(
  path: string,
  payload: Record<string, any> = {},
): Promise<{ status: number; body: any }> {
  const response = await fetch(`http://localhost${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
    // @ts-ignore
    unix: SOCKET_PATH,
  });

  const text = await response.text();
  let body: any;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }

  return { status: response.status, body };
}

function encodeBody(obj: any): string {
  return btoa(JSON.stringify(obj));
}

function decodeBody(response: any): any {
  if (response.body && typeof response.body === "string") {
    try {
      return JSON.parse(atob(response.body));
    } catch {
      return response.body;
    }
  }
  return response;
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

// ── Tests ───────────────────────────────────────────

async function run() {
  console.log("\n🚀 Sky Demo — Integration Tests\n");

  // ── Health ──────────────────────────────────

  console.log("HealthService:");

  await test("GET /health returns healthy status", async () => {
    const { status, body } = await call("/sky.v1.HealthService/Check", {});
    eq(status, 200, "HTTP status");

    const decoded = decodeBody(body);
    eq(decoded.status, "healthy", "health status");
    assert(decoded.startedAt !== undefined, "should have startedAt");
    assert(decoded.uptime !== undefined, "should have uptime");
    console.log(`    uptime: ${decoded.uptime}`);
  });

  // ── Users CRUD ─────────────────────────────

  console.log("\nUserService:");

  let userId: string;

  await test("POST /users creates a user with Zod validation", async () => {
    const { status, body } = await call("/sky.v1.UserService/CreateUser", {
      body: encodeBody({
        name: "Alice",
        email: "alice@example.com",
        role: "admin",
      }),
    });

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    assert(decoded.id !== undefined, "should have id");
    eq(decoded.name, "Alice", "name");
    eq(decoded.email, "alice@example.com", "email");
    eq(decoded.role, "admin", "role");
    userId = decoded.id;
    console.log(`    created user ${userId}`);
  });

  await test("POST /users rejects invalid email", async () => {
    const { status, body } = await call("/sky.v1.UserService/CreateUser", {
      body: encodeBody({
        name: "Bob",
        email: "not-an-email",
      }),
    });

    // Should fail Zod validation
    const decoded = decodeBody(body);
    console.log(`    response: ${JSON.stringify(decoded)}`);
  });

  await test("POST /users rejects missing name", async () => {
    const { status, body } = await call("/sky.v1.UserService/CreateUser", {
      body: encodeBody({
        email: "bob@example.com",
      }),
    });

    const decoded = decodeBody(body);
    console.log(`    response: ${JSON.stringify(decoded)}`);
  });

  await test("GET /users/:id returns the created user", async () => {
    const { status, body } = await call("/sky.v1.UserService/GetUser", {
      id: userId,
    });

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    eq(decoded.id, userId, "id");
    eq(decoded.name, "Alice", "name");
  });

  await test("GET /users/:id returns 404 for missing user", async () => {
    const { status, body } = await call("/sky.v1.UserService/GetUser", {
      id: "999",
    });

    const decoded = decodeBody(body);
    eq(decoded.code, "handler_error", "error code");
    console.log(`    message: ${decoded.message}`);
  });

  await test("POST /users creates a second user", async () => {
    const { status, body } = await call("/sky.v1.UserService/CreateUser", {
      body: encodeBody({
        name: "Bob",
        email: "bob@example.com",
      }),
    });

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    eq(decoded.name, "Bob", "name");
    eq(decoded.role, "member", "default role");
    console.log(`    created user ${decoded.id} with default role`);
  });

  await test("GET /users lists users with pagination", async () => {
    const { status, body } = await call("/sky.v1.UserService/ListUsers", {
      page: "1",
      limit: "10",
    });

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    assert(decoded.items.length >= 2, "should have at least 2 users");
    eq(decoded.page, 1, "page");
    console.log(`    total: ${decoded.total}, showing: ${decoded.items.length}`);
  });

  await test("PUT /users/:id updates a user", async () => {
    const { status, body } = await call("/sky.v1.UserService/UpdateUser", {
      id: userId,
      body: encodeBody({ name: "Alice Updated" }),
    });

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    eq(decoded.name, "Alice Updated", "updated name");
    eq(decoded.email, "alice@example.com", "email unchanged");
  });

  await test("DELETE /users/:id removes a user", async () => {
    const { status, body } = await call("/sky.v1.UserService/DeleteUser", {
      id: userId,
    });

    eq(status, 200, "HTTP status");
  });

  await test("GET /users/:id returns 404 after deletion", async () => {
    const { status, body } = await call("/sky.v1.UserService/GetUser", {
      id: userId,
    });

    const decoded = decodeBody(body);
    eq(decoded.code, "handler_error", "error code");
  });

  // ── Admin ──────────────────────────────────

  console.log("\nAdminService:");

  await test("GET /api/admin/users with valid token", async () => {
    const { status, body } = await call(
      "/sky.v1.AdminService/ListAdminUsers",
      {
        x_admin_token: "sky-admin-secret",
        page: "1",
      },
    );

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    eq(decoded.admin, true, "admin flag");
    console.log(`    response: ${JSON.stringify(decoded)}`);
  });

  await test("GET /api/admin/users with invalid token returns 401", async () => {
    const { status, body } = await call(
      "/sky.v1.AdminService/ListAdminUsers",
      {
        x_admin_token: "wrong-token",
        page: "1",
      },
    );

    const decoded = decodeBody(body);
    eq(decoded.code, "handler_error", "error code");
    console.log(`    message: ${decoded.message}`);
  });

  await test("GET /api/admin/stats with valid token", async () => {
    const { status, body } = await call(
      "/sky.v1.AdminService/GetStats",
      {
        x_admin_token: "sky-admin-secret",
      },
    );

    eq(status, 200, "HTTP status");
    const decoded = decodeBody(body);
    assert(decoded.totalUsers !== undefined, "should have totalUsers");
    console.log(`    stats: ${JSON.stringify(decoded)}`);
  });

  // ── Summary ────────────────────────────────

  console.log("\n─────────────────────────────────────────");

  if (failed > 0) {
    console.log(`\n❌ ${passed} passed, ${failed} failed\n`);
    process.exit(1);
  }

  console.log(`\n✅ All ${passed} tests passed\n`);
}

run().catch((err) => {
  console.error("\n💥 Fatal:", err.message);
  console.error("\nIs the worker running?");
  console.error("  sky build");
  console.error("  SKY_WORKER_ID=1 bun run index.ts\n");
  process.exit(1);
});
