/**
 * Proto Emitter
 *
 * Reads sky-manifest.json and generates per-service .proto files.
 *
 * Design decisions:
 * - One .proto file per service (UserService → user_service.proto)
 * - One RPC per handler method
 * - Request messages: `bytes body` for Body() extracts, typed `string`
 *   fields for Param/Query/Header extracts
 * - All RPCs return SkyResponse (status, body bytes, headers, cookies)
 * - SkyResponse and SetCookie are defined in a shared sky_response.proto
 *   that each service proto imports
 *
 * Usage:
 *   const emitter = new ProtoEmitter(manifest, { outDir: "./proto/generated" });
 *   await emitter.emit();
 */

import { writeFile, mkdir } from "fs/promises";
import { join } from "path";

// ── Types ───────────────────────────────────────────────

import type { 
  ManifestExtract,
  ManifestService
} from "./assembler";

export interface Manifest {
  version: string;
  services: ManifestService[];
  schemas: Record<string, unknown>;
}

interface ProtoEmitterOptions {
  /** Output directory for generated .proto files */
  outDir: string;

  /** Proto package name. Default: "sky.v1" */
  packageName?: string;

  /** Overwrite existing files. Default: true */
  overwrite?: boolean;
}

// ── Shared proto ────────────────────────────────────────

const SKY_RESPONSE_PROTO = `syntax = "proto3";

package sky.v1;

// Shared response type returned by all Sky handler RPCs.
//
// Handlers return a Response object (or a plain value that Sky
// wraps automatically). The dispatcher serializes it into this
// message. The gateway reads status, body, headers, and cookies
// to build the HTTP response.

message SkyResponse {
  // HTTP status code.
  uint32 status = 1;

  // JSON-serialized response body.
  bytes body = 2;

  // Additional response headers to merge into the HTTP response.
  map<string, string> headers = 3;

  // Cookies to set on the HTTP response.
  repeated SetCookie cookies = 4;
}

message SetCookie {
  string name = 1;
  string value = 2;
  optional uint32 max_age = 3;
  optional string path = 4;
  optional string domain = 5;
  bool http_only = 6;
  bool secure = 7;
  optional string same_site = 8;
}
`;

// ── Emitter ─────────────────────────────────────────────

export class ProtoEmitter {
  private manifest: Manifest;
  private options: Required<ProtoEmitterOptions>;

  constructor(manifest: Manifest, options: ProtoEmitterOptions) {
    this.manifest = manifest;
    this.options = {
      packageName: "sky.v1",
      overwrite: true,
      ...options,
    };
  }

  /**
   * Generate all .proto files from the manifest.
   *
   * Produces:
   * - sky_response.proto (shared response type)
   * - One .proto per service (e.g., user_service.proto)
   *
   * Returns the list of generated file paths.
   */
  async emit(): Promise<string[]> {
    await mkdir(this.options.outDir, { recursive: true });

    const files: string[] = [];

    // 1. Write shared response proto.
    const responsePath = join(this.options.outDir, "sky_response.proto");
    await writeFile(responsePath, SKY_RESPONSE_PROTO, "utf-8");
    files.push(responsePath);

    // 2. Generate per-service protos.
    for (const service of this.manifest.services) {
      const proto = this.generateServiceProto(service);
      const fileName = this.toSnakeCase(service.className) + ".proto";
      const filePath = join(this.options.outDir, fileName);
      await writeFile(filePath, proto, "utf-8");
      files.push(filePath);
    }

    return files;
  }

  /**
   * Generate the .proto file content for a single service.
   */
  private generateServiceProto(service: ManifestService): string {
    const lines: string[] = [];

    // Header
    lines.push(`syntax = "proto3";`);
    lines.push(``);
    lines.push(`package ${this.options.packageName};`);
    lines.push(``);
    lines.push(`import "sky_response.proto";`);
    lines.push(``);

    // Collect all messages first so we can emit them after the service.
    const messages: string[] = [];
    const rpcLines: string[] = [];

    for (const handler of service.handlers) {
      const rpcName = this.toPascalCase(handler.name);
      const requestName = `${rpcName}Request`;

      // Build request message.
      const msg = this.generateRequestMessage(requestName, handler.extract);
      messages.push(msg);

      // Build RPC line.
      rpcLines.push(
        `  rpc ${rpcName}(${requestName}) returns (SkyResponse);`
      );
    }

    // Service definition
    lines.push(`service ${service.className} {`);
    for (const rpc of rpcLines) {
      lines.push(rpc);
    }
    lines.push(`}`);
    lines.push(``);

    // Message definitions
    for (const msg of messages) {
      lines.push(msg);
    }

    return lines.join("\n");
  }

  /**
   * Generate a request message for a handler.
   *
   * - Body() extract → `bytes body` field
   * - Param(name) → `string {name}` field
   * - Query(name) → `string {name}` field
   * - Header(name) → `string {name}` field
   *
   * Field numbers are assigned sequentially. Body is always
   * field 1 if present, then scalars follow in extract order.
   */
  private generateRequestMessage(
    messageName: string,
    extracts: ManifestExtract[]
  ): string {
    const lines: string[] = [];
    lines.push(`message ${messageName} {`);

    let fieldNumber = 1;
    const seenFields = new Set<string>();

    for (const extract of extracts) {
      if (extract.source === "body") {
        lines.push(`  bytes body = ${fieldNumber};`);
        fieldNumber++;
      } else if (extract.name) {
        // Sanitize field name for proto compatibility.
        const fieldName = this.toProtoFieldName(extract.name);

        // Avoid duplicate field names (e.g., same param name
        // from different sources — unlikely but defensive).
        if (seenFields.has(fieldName)) {
          const qualifiedName = `${extract.source}_${fieldName}`;
          lines.push(`  string ${qualifiedName} = ${fieldNumber};`);
          seenFields.add(qualifiedName);
        } else {
          lines.push(`  string ${fieldName} = ${fieldNumber};`);
          seenFields.add(fieldName);
        }
        fieldNumber++;
      }
    }

    // Empty message if no extracts (e.g., health check).
    lines.push(`}`);
    lines.push(``);

    return lines.join("\n");
  }

  // ── Naming helpers ──────────────────────────────

  /**
   * Convert a class name to snake_case for file names.
   * UserService → user_service
   */
  private toSnakeCase(name: string): string {
    return name
      .replace(/([A-Z])/g, (match, char, index) =>
        index > 0 ? `_${char}` : char
      )
      .toLowerCase();
  }

  /**
   * Convert a method name to PascalCase for RPC names.
   * createUser → CreateUser
   * getUser → GetUser
   */
  private toPascalCase(name: string): string {
    return name.charAt(0).toUpperCase() + name.slice(1);
  }

  /**
   * Sanitize a parameter name for use as a proto field name.
   *
   * Proto field names must be lowercase letters, digits, and
   * underscores, starting with a letter.
   *
   * x-request-id → x_request_id
   * page → page
   */
  private toProtoFieldName(name: string): string {
    return name
      .replace(/[^a-zA-Z0-9]/g, "_")
      .replace(/^[^a-zA-Z]/, (c) => `f_${c}`)
      .toLowerCase();
  }
}
