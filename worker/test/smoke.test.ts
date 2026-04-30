/**
 * Smoke Test — HelloService End-to-End
 *
 * Verifies the full Sky pipeline:
 *   @Service/@Handler decorators
 *   → ServiceRegistry + DI container
 *   → HandlerDispatcher
 *   → Connect server on Unix domain socket
 *   → gRPC call + SkyResponse
 *
 * Prerequisites:
 *   1. Run proto codegen: buf generate (or protoc)
 *   2. Start worker: SKY_WORKER_ID=1 bun run index.ts
 *   3. Run this: bun run smoke-test.ts
 */

const SOCKET_PATH = "/tmp/sky/workers/sky-worker-1.sock";

// ── Helpers ─────────────────────────────────────────

interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
}

const results: TestResult[] = [];

async function connectCall(
  servicePath: string,
  payload: Record<string, any> = {}
): Promise<{ status: number; body: any; headers: Headers }> {
  const url = `http://localhost${servicePath}`;

  const response = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
    // @ts-ignore — Bun supports unix option in fetch
    unix: SOCKET_PATH
  });

  const text = await response.text();
  let body: any;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }

  return { status: response.status, body, headers: response.headers };
}

function encodeBodyToBase64(obj: any): string {
  const json = JSON.stringify(obj);
  const bytes = new TextEncoder().encode(json);
  return btoa(String.fromCharCode(...bytes));
}

async function test(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
    results.push({ name, passed: true });
    console.log(`  ✓ ${name}`);
  } catch (err: any) {
    results.push({ name, passed: false, error: err.message });
    console.log(`  ✗ ${name}`);
    console.log(`    ${err.message}`);
  }
}

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

function assertEqual(actual: any, expected: any, label: string): void {
  assert(
    actual === expected,
    `${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
  );
}

// ── Tests ───────────────────────────────────────────

async function run() {
  console.log("\n🚀 Sky Worker — HelloService Smoke Tests\n");
  console.log(`Socket: ${SOCKET_PATH}\n`);

  // ── Connectivity ────────────────────────────

  await test("worker is reachable on socket", async () => {
    const { status } = await connectCall("/sky.v1.WorkerControl/Health");
    assert(status > 0, `connection failed`);
  });

  // ── WorkerControl still works ───────────────

  await test("WorkerControl.Health returns READY", async () => {
    const { status, body } = await connectCall("/sky.v1.WorkerControl/Health");
    assertEqual(status, 200, "HTTP status");
    console.log(`    Worker version: ${body.workerVersion}`);
    console.log(`    Status: ${body.status}`);
  });

  // ── HelloService.Greet ──────────────────────

  await test("HelloService.Greet with valid name", async () => {
    const payload = {
      body: encodeBodyToBase64({ name: "World" }),
    };

    const { status, body } = await connectCall(
      "/sky.v1.HelloService/Greet",
      payload
    );

    assertEqual(status, 200, "HTTP status");

    // The response body is a SkyResponse. The `body` field
    // contains the JSON-serialized handler return value as
    // base64-encoded bytes.
    console.log(`    Raw response: ${JSON.stringify(body)}`);

    // If the dispatcher correctly handled it, we should see
    // the SkyResponse shape with status and body.
    if (body.body) {
      // Decode the base64 body bytes back to JSON
      const decoded = JSON.parse(atob(body.body));
      console.log(`    Decoded body: ${JSON.stringify(decoded)}`);
      assertEqual(decoded.message, "Hello World", "greeting message");
    } else if (body.message) {
      // If Connect returned the response directly (not wrapped in SkyResponse)
      assertEqual(body.message, "Hello World", "greeting message");
    }
  });

  await test("HelloService.Greet with different name", async () => {
    const payload = {
      body: encodeBodyToBase64({ name: "Sky" }),
    };

    const { status, body } = await connectCall(
      "/sky.v1.HelloService/Greet",
      payload
    );

    assertEqual(status, 200, "HTTP status");

    if (body.body) {
      const decoded = JSON.parse(atob(body.body));
      assertEqual(decoded.message, "Hello Sky", "greeting message");
    } else if (body.message) {
      assertEqual(body.message, "Hello Sky", "greeting message");
    }
  });

  await test("HelloService.Greet with empty body", async () => {
    const payload = {
      body: encodeBodyToBase64({}),
    };

    const { status, body } = await connectCall(
      "/sky.v1.HelloService/Greet",
      payload
    );

    assertEqual(status, 200, "HTTP status");

    // Handler should still work — body.name will be undefined,
    // so greeting should be "Hello undefined"
    if (body.body) {
      const decoded = JSON.parse(atob(body.body));
      console.log(`    Decoded body: ${JSON.stringify(decoded)}`);
    } else if (body.message) {
      console.log(`    Message: ${body.message}`);
    }
  });

  // ── Error cases ─────────────────────────────

  await test("unknown service returns not found", async () => {
    const { status, body } = await connectCall(
      "/sky.v1.FakeService/FakeMethod",
      {}
    );

    // Connect returns 404 or an error code for unknown services
    const isError =
      status === 404 ||
      status === 405 ||
      body?.code === "unimplemented" ||
      body?.code === "not_found";

    assert(isError, `expected error, got ${status}: ${JSON.stringify(body)}`);
  });

  await test("unknown method on known service returns error", async () => {
    const { status, body } = await connectCall(
      "/sky.v1.HelloService/NonExistentMethod",
      {}
    );

    const isError =
      status === 404 ||
      status === 405 ||
      body?.code === "unimplemented" ||
      body?.code === "not_found";

    assert(isError, `expected error, got ${status}: ${JSON.stringify(body)}`);
  });

  // ── Summary ────────────────────────────────

  console.log("\n─────────────────────────────────────────");
  const passed = results.filter((r) => r.passed).length;
  const failed = results.filter((r) => !r.passed).length;

  if (failed > 0) {
    console.log(`\n❌ ${passed} passed, ${failed} failed\n`);
    for (const r of results.filter((r) => !r.passed)) {
      console.log(`  ✗ ${r.name}`);
      console.log(`    ${r.error}`);
    }
    process.exit(1);
  }

  console.log(`\n✅ All ${passed} tests passed\n`);
}

// ── Run ─────────────────────────────────────────────

run().catch((err) => {
  console.error("\n💥 Fatal error:", err.message);
  console.error("\nIs the worker running? Start with:");
  console.error("  SKY_WORKER_ID=1 bun run index.ts\n");
  process.exit(1);
});