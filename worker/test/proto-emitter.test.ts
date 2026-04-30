import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { readFile, rm, readdir } from "fs/promises";
import { join } from "path";
import { ProtoEmitter, type Manifest } from "@emitter";

const FIXTURE_PATH = join(import.meta.dir, "./fixtures/app/sky-manifest-fixture.json");
const OUT_DIR = join(import.meta.dir, "./fixtures/app/.proto-out");

let manifest: any;
let emittedFiles: string[];

beforeAll(async () => {
  const raw = await readFile(FIXTURE_PATH, "utf-8");
  manifest = JSON.parse(raw);

  const emitter = new ProtoEmitter(manifest, { outDir: OUT_DIR });
  emittedFiles = await emitter.emit();
});

// ── File generation ───────────────────────────────────

describe("file generation", () => {
  it("generates the correct number of files", () => {
    // 1 shared sky_response.proto + 3 services
    expect(emittedFiles).toHaveLength(4);
  });

  it("generates sky_response.proto", () => {
    const names = emittedFiles.map((f) => f.split("/").pop());
    expect(names).toContain("sky_response.proto");
  });

  it("generates one proto per service", () => {
    const names = emittedFiles.map((f) => f.split("/").pop());
    expect(names).toContain("user_service.proto");
    expect(names).toContain("health_service.proto");
    expect(names).toContain("admin_service.proto");
  });

  it("writes files to the output directory", async () => {
    const dirContents = await readdir(OUT_DIR);
    expect(dirContents).toContain("sky_response.proto");
    expect(dirContents).toContain("user_service.proto");
    expect(dirContents).toContain("health_service.proto");
    expect(dirContents).toContain("admin_service.proto");
  });
});

// ── SkyResponse proto ─────────────────────────────────

describe("sky_response.proto", () => {
  let content: string;

  beforeAll(async () => {
    content = await readFile(join(OUT_DIR, "sky_response.proto"), "utf-8");
  });

  it("declares the correct package", () => {
    expect(content).toContain("package sky.v1;");
  });

  it("defines SkyResponse message", () => {
    expect(content).toContain("message SkyResponse {");
  });

  it("includes status field", () => {
    expect(content).toContain("uint32 status = 1;");
  });

  it("includes body as bytes", () => {
    expect(content).toContain("bytes body = 2;");
  });

  it("includes headers map", () => {
    expect(content).toContain("map<string, string> headers = 3;");
  });

  it("includes cookies as repeated SetCookie", () => {
    expect(content).toContain("repeated SetCookie cookies = 4;");
  });

  it("defines SetCookie message", () => {
    expect(content).toContain("message SetCookie {");
    expect(content).toContain("string name = 1;");
    expect(content).toContain("string value = 2;");
    expect(content).toContain("optional uint32 max_age = 3;");
    expect(content).toContain("bool http_only = 6;");
    expect(content).toContain("bool secure = 7;");
    expect(content).toContain('optional string same_site = 8;');
  });
});

// ── UserService proto ─────────────────────────────────

describe("user_service.proto", () => {
  let content: string;

  beforeAll(async () => {
    content = await readFile(join(OUT_DIR, "user_service.proto"), "utf-8");
  });

  it("declares proto3 syntax", () => {
    expect(content).toContain('syntax = "proto3";');
  });

  it("declares the correct package", () => {
    expect(content).toContain("package sky.v1;");
  });

  it("imports sky_response.proto", () => {
    expect(content).toContain('import "sky_response.proto";');
  });

  it("defines the UserService service", () => {
    expect(content).toContain("service UserService {");
  });

  it("generates RPC for each handler", () => {
    expect(content).toContain("rpc CreateUser(CreateUserRequest) returns (SkyResponse);");
    expect(content).toContain("rpc GetUser(GetUserRequest) returns (SkyResponse);");
    expect(content).toContain("rpc ListUsers(ListUsersRequest) returns (SkyResponse);");
    expect(content).toContain("rpc UpdateUser(UpdateUserRequest) returns (SkyResponse);");
    expect(content).toContain("rpc DeleteUser(DeleteUserRequest) returns (SkyResponse);");
  });

  it("CreateUserRequest has body as bytes", () => {
    const msg = extractMessage(content, "CreateUserRequest");
    expect(msg).toContain("bytes body = 1;");
  });

  it("CreateUserRequest has no scalar fields", () => {
    const msg = extractMessage(content, "CreateUserRequest");
    expect(msg).not.toContain("string ");
  });

  it("GetUserRequest has typed id field", () => {
    const msg = extractMessage(content, "GetUserRequest");
    expect(msg).toContain("string id = 1;");
  });

  it("GetUserRequest has no body field", () => {
    const msg = extractMessage(content, "GetUserRequest");
    expect(msg).not.toContain("bytes body");
  });

  it("ListUsersRequest has typed query fields", () => {
    const msg = extractMessage(content, "ListUsersRequest");
    expect(msg).toContain("string page = 1;");
    expect(msg).toContain("string limit = 2;");
  });

  it("UpdateUserRequest has both param and body", () => {
    const msg = extractMessage(content, "UpdateUserRequest");
    expect(msg).toContain("string id = 1;");
    expect(msg).toContain("bytes body = 2;");
  });

  it("DeleteUserRequest has typed id field", () => {
    const msg = extractMessage(content, "DeleteUserRequest");
    expect(msg).toContain("string id = 1;");
  });
});

// ── HealthService proto ───────────────────────────────

describe("health_service.proto", () => {
  let content: string;

  beforeAll(async () => {
    content = await readFile(join(OUT_DIR, "health_service.proto"), "utf-8");
  });

  it("defines the HealthService service", () => {
    expect(content).toContain("service HealthService {");
  });

  it("generates RPC for check handler", () => {
    expect(content).toContain("rpc Check(CheckRequest) returns (SkyResponse);");
  });

  it("CheckRequest is an empty message", () => {
    const msg = extractMessage(content, "CheckRequest");
    const fields = msg
      .split("\n")
      .filter((l) => l.trim() && !l.includes("message") && !l.includes("}"));
    expect(fields).toHaveLength(0);
  });
});

// ── AdminService proto ────────────────────────────────

describe("admin_service.proto", () => {
  let content: string;

  beforeAll(async () => {
    content = await readFile(join(OUT_DIR, "admin_service.proto"), "utf-8");
  });

  it("defines the AdminService service", () => {
    expect(content).toContain("service AdminService {");
  });

  it("generates RPC for listAdminUsers", () => {
    expect(content).toContain(
      "rpc ListAdminUsers(ListAdminUsersRequest) returns (SkyResponse);"
    );
  });

  it("sanitizes header name with hyphens", () => {
    const msg = extractMessage(content, "ListAdminUsersRequest");
    // x-admin-token → x_admin_token
    expect(msg).toContain("string x_admin_token = 1;");
    expect(msg).not.toContain("x-admin-token");
  });

  it("includes query param after header", () => {
    const msg = extractMessage(content, "ListAdminUsersRequest");
    expect(msg).toContain("string page = 2;");
  });
});

// ── Edge cases ────────────────────────────────────────

describe("edge cases", () => {
  it("handles custom package name", async () => {
    const customOutDir = join(OUT_DIR, "custom");
    const emitter = new ProtoEmitter(manifest, {
      outDir: customOutDir,
      packageName: "myapp.api.v2",
    });

    await emitter.emit();

    const content = await readFile(
      join(customOutDir, "user_service.proto"),
      "utf-8"
    );
    expect(content).toContain("package myapp.api.v2;");

    await rm(customOutDir, { recursive: true, force: true });
  });

  it("handles manifest with no services", async () => {
    const emptyManifest = {
      version: "1",
      services: [],
      schemas: {},
    };

    const emptyOutDir = join(OUT_DIR, "empty");
    const emitter = new ProtoEmitter(emptyManifest, { outDir: emptyOutDir });
    const files = await emitter.emit();

    // Only sky_response.proto
    expect(files).toHaveLength(1);

    await rm(emptyOutDir, { recursive: true, force: true });
  });

  it("handles service with no handlers", async () => {
    const noHandlerManifest = {
      version: "1",
      services: [
        {
          name: "emptyService",
          className: "EmptyService",
          lifetime: "singleton",
          dependencies: [],
          handlers: [],
        },
      ],
      schemas: {},
    } satisfies Manifest;

    const noHandlerOutDir = join(OUT_DIR, "no-handler");
    const emitter = new ProtoEmitter(noHandlerManifest, {
      outDir: noHandlerOutDir,
    });
    await emitter.emit();

    const content = await readFile(
      join(noHandlerOutDir, "empty_service.proto"),
      "utf-8"
    );
    expect(content).toContain("service EmptyService {");
    // Service block should be empty (no RPCs).
    const serviceBlock = content.substring(
      content.indexOf("service EmptyService {"),
      content.indexOf("}", content.indexOf("service EmptyService {")) + 1
    );
    expect(serviceBlock).not.toContain("rpc ");

    await rm(noHandlerOutDir, { recursive: true, force: true });
  });
});

// ── Helpers ───────────────────────────────────────────

/**
 * Extract a message block from proto content by name.
 * Returns the content between `message Name {` and its closing `}`.
 */
function extractMessage(proto: string, name: string): string {
  const startMarker = `message ${name} {`;
  const startIdx = proto.indexOf(startMarker);
  if (startIdx === -1) {
    throw new Error(`Message ${name} not found in proto:\n${proto}`);
  }

  let depth = 0;
  let endIdx = startIdx;

  for (let i = startIdx; i < proto.length; i++) {
    if (proto[i] === "{") depth++;
    if (proto[i] === "}") {
      depth--;
      if (depth === 0) {
        endIdx = i + 1;
        break;
      }
    }
  }

  return proto.substring(startIdx, endIdx);
}