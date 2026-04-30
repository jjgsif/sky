/**
 * Handler Dispatcher
 *
 * Receives per-service proto request messages from the Connect server,
 * resolves the target service from the DI container, constructs the
 * argument list from the proto fields, calls the handler method, and
 * serializes the return value into a SkyResponse.
 *
 * Each invocation gets its own DI scope (container.createScope()),
 * so request-scoped services are fresh per request while singletons
 * are shared across all requests.
 *
 * Handlers can return:
 * - A plain object → wrapped with default status, serialized to JSON bytes
 * - A Response instance → status, headers, cookies extracted explicitly
 * - undefined/null → empty body
 */

import { Container } from "@blue.ts/di";
import type { ConnectRouter } from "@connectrpc/connect";
import type { GenService } from "@bufbuild/protobuf/codegenv2";
import type { ServiceRegistry } from "./service-registry";
import type { ExtractDescriptor } from "@decorators";
import { Response } from "./response";
import type { SetCookieDescriptor } from "./response";
import { logger } from "./logger";
import { SkyResponseSchema } from "@gen/sky_response_pb";
import { create } from "@bufbuild/protobuf";

// ── Error types ─────────────────────────────────────────

/**
 * Framework-provided error for handlers to throw with a
 * specific HTTP status code.
 *
 * @example
 * throw new HttpError(404, "User not found");
 * throw new HttpError(409, "Email already exists");
 */
export class HttpError extends Error {
  constructor(
    public readonly status: number,
    message: string
  ) {
    super(message);
    this.name = "HttpError";
  }
}

// ── Types ───────────────────────────────────────────────

/**
 * The shape of a SkyResponse proto message.
 * Matches sky_response.proto.
 */
export interface SkyResponse extends Record<string, unknown> {
  status: number;
  body: Uint8Array;
  headers: Record<string, string>;
  cookies: SkyResponseCookie[];
}

export interface SkyResponseCookie {
  name: string;
  value: string;
  maxAge?: number;
  path?: string;
  domain?: string;
  httpOnly: boolean;
  secure: boolean;
  sameSite?: string;
}

/**
 * Decoded request fields from the per-service proto message.
 * The Connect server decodes the typed proto into this shape
 * before handing off to the dispatcher.
 */
export interface DecodedRequest {
  /** Handler identity: "serviceName::handlerName" */
  handlerId: string;

  /** The raw body bytes (from `bytes body` field), or empty. */
  body: Uint8Array;

  /** Scalar fields extracted from the proto message, keyed by
   *  field name (matching the extract descriptor names). */
  fields: Record<string, string>;
}

// ── Dispatcher ──────────────────────────────────────────

export class HandlerDispatcher {
  /** Map of service class name → generated Connect service type.
   *  Populated at startup via registerServiceType(). */
  private serviceTypes = new Map<string, GenService<any>>();

  constructor(
    private container: Container,
    private registry: ServiceRegistry
  ) { }

  /**
   * Register a generated Connect service type for a service class.
   *
   * Call this at startup for each service, passing the generated
   * service definition from the compiled proto:
   *
   * @example
   * import { UserService } from "./gen/sky/v1/user_service_connect";
   * dispatcher.registerServiceType("UserService", UserService);
   */
  registerServiceType(className: string, serviceType: GenService<any>): void {
    this.serviceTypes.set(className, serviceType);
  }

  /**
   * Register all per-service Connect handlers with a router.
   *
   * For each registered service type, creates a Connect service
   * implementation where every RPC method:
   *   1. Decodes the proto request into a DecodedRequest
   *   2. Dispatches to the handler via the DI container
   *   3. Returns the SkyResponse
   *
   * Called from server.ts during startup.
   */
  registerServices(router: ConnectRouter): void {
    for (const [className, serviceType] of this.serviceTypes) {
      const serviceInfo = this.registry.getServiceInfoByClassName(className);
      if (!serviceInfo) {
        logger.warn({ className }, "service type registered but not found in registry");
        continue;
      }

      const serviceName = serviceInfo.name;

      // Build a handler implementation object where each method
      // name maps to an async function that dispatches via DI.
      const implementation: Record<string, (req: any) => Promise<SkyResponse>> = {};

      const handlers = serviceInfo.registration.handlers;
      for (const [handlerName, handlerDef] of handlers) {
        // Connect uses the RPC name from the proto, which is the
        // handler name with first letter lowercased (Connect convention).
        // The proto emitter capitalizes it, Connect lowercases it back.
        const rpcMethodName = handlerName.charAt(0).toLowerCase() + handlerName.slice(1);

        implementation[rpcMethodName] = async (req: any) => {
          // Decode the typed proto request into a DecodedRequest.
          const decoded = this.decodeProtoRequest(
            serviceName,
            handlerName,
            req
          );

          return this.dispatch(decoded);
        };
      }

      router.service(serviceType, implementation);

      logger.info(
        { className, handlers: handlers.size },
        "registered Connect service"
      );
    }
  }

  /**
   * Decode a generated proto request message into a DecodedRequest.
   *
   * The generated proto message has typed fields:
   * - `body` (Uint8Array) for Body() extracts
   * - String fields for Param/Query/Header extracts
   *
   * We read the extract descriptors to know which fields to pull
   * from the proto message and map them into the generic
   * DecodedRequest shape the dispatcher understands.
   */
  private decodeProtoRequest(
    serviceName: string,
    handlerName: string,
    req: any
  ): DecodedRequest {
    const extracts = this.registry.getExtracts(serviceName, handlerName);
    const fields: Record<string, string> = {};
    let body = new Uint8Array(0);

    for (const extract of extracts) {
      if (extract.source === "body") {
        // The proto field is named "body" and typed as bytes.
        body = req.body ?? new Uint8Array(0);
      } else if (extract.name) {
        // Scalar fields — the proto field name is the sanitized
        // version of the extract name (hyphens → underscores).
        const protoFieldName = extract.name.replace(/[^a-zA-Z0-9]/g, "_").toLowerCase();
        const value = req[protoFieldName];
        if (value !== undefined && value !== "") {
          fields[extract.name] = String(value);
        }
      }
    }

    return {
      handlerId: `${serviceName}::${handlerName}`,
      body,
      fields,
    };
  }

  /**
   * Dispatch a decoded request to the appropriate service method.
   */
  async dispatch(request: DecodedRequest): Promise<SkyResponse> {
    const { serviceName, handlerName } = this.parseHandlerId(
      request.handlerId
    );

    const log = logger.child({
      handler: request.handlerId,
    });

    log.debug("dispatching handler invocation");

    // ── Look up service and handler ───────────
    const cls = this.registry.getServiceClass(serviceName);
    if (!cls) {
      log.warn({ serviceName }, "service not found in registry");
      return this.errorResponse(
        404,
        "handler_not_found",
        `Service '${serviceName}' not found`
      );
    }

    const handlerDef = this.registry.getHandlerDefinition(
      serviceName,
      handlerName
    );
    if (!handlerDef) {
      log.warn({ handlerName }, "handler method not found on service");
      return this.errorResponse(
        404,
        "handler_not_found",
        `Handler '${handlerName}' not found on service '${serviceName}'`
      );
    }

    // ── Resolve service from DI ───────────────
    const scope = this.container.createScope();

    let service: any;
    try {
      service = await scope.get(cls);
    } catch (err) {
      log.error({ err }, "failed to resolve service from DI container");
      return this.errorResponse(
        500,
        "di_resolution_failed",
        "Failed to resolve service dependencies"
      );
    }

    if (typeof service[handlerName] !== "function") {
      log.error(
        {
          handlerName,
          available: Object.getOwnPropertyNames(
            Object.getPrototypeOf(service)
          ),
        },
        "handler method not callable on service instance"
      );
      return this.errorResponse(
        500,
        "handler_not_callable",
        `Method '${handlerName}' is not a function on '${serviceName}'`
      );
    }

    // ── Bind parameters ───────────────────────
    const extracts = this.registry.getExtracts(serviceName, handlerName);
    const args = this.bindParameters(request, extracts);

    // ── Call handler ──────────────────────────
    try {
      const result = await service[handlerName](...args);
      return this.buildResponse(result, handlerDef.status);
    } catch (err) {
      return this.handleError(err, log);
    }
  }

  /**
   * Parse "serviceName::handlerName" into its components.
   */
  private parseHandlerId(handlerId: string): {
    serviceName: string;
    handlerName: string;
  } {
    const separatorIndex = handlerId.indexOf("::");
    if (separatorIndex === -1) {
      throw new Error(
        `Invalid handler_id format: '${handlerId}' (expected 'service::handler')`
      );
    }

    return {
      serviceName: handlerId.substring(0, separatorIndex),
      handlerName: handlerId.substring(separatorIndex + 2),
    };
  }

  /**
   * Construct the argument array for the handler method.
   *
   * Position is array index — extracts[0] is arg 0, etc.
   *
   * - Body → parsed JSON from request.body
   * - Param/Query/Header → string from request.fields
   */
  private bindParameters(
    request: DecodedRequest,
    extracts: ExtractDescriptor[]
  ): any[] {
    if (extracts.length === 0) {
      return [];
    }

    // Pre-parse body once if needed.
    let parsedBody: any = undefined;
    const needsBody = extracts.some((e) => e.source === "body");

    if (needsBody && request.body.length > 0) {
      try {
        const bodyStr = new TextDecoder().decode(request.body);
        parsedBody = JSON.parse(bodyStr);
      } catch {
        parsedBody = undefined;
      }
    }

    return extracts.map((extract) => {
      switch (extract.source) {
        case "body":
          return parsedBody;

        case "param":
        case "query":
        case "header":
          return extract.name ? request.fields[extract.name] : undefined;

        default:
          return undefined;
      }
    });
  }

  /**
   * Build a SkyResponse from a handler's return value.
   *
   * - Response instance → extract status, body, headers, cookies
   * - Plain object → serialize to JSON, use default status
   * - undefined/null → empty body, use default status
   */
  private buildResponse(result: any, defaultStatus: number): SkyResponse {
    let response;
    if (result instanceof Response) {
      const body = result.getBody();
      const status = result.getStatus() || defaultStatus;

      response = {
        status,
        body: this.serializeBody(body),
        headers: result.getHeaders(),
        cookies: result.getCookies().map(this.mapCookie),
      };
    }

    // Plain object or primitive.
    response = {
      status: defaultStatus,
      body: this.serializeBody(result),
      headers: {},
      cookies: [],
    };
    return create(SkyResponseSchema, response);
  }

  /**
   * Serialize a value to JSON bytes.
   */
  private serializeBody(value: any): Uint8Array {
    if (value === undefined || value === null) {
      return new Uint8Array(0);
    }

    const json = JSON.stringify(value);
    return new TextEncoder().encode(json);
  }

  /**
   * Map a SetCookieDescriptor to the proto cookie shape.
   */
  private mapCookie(cookie: SetCookieDescriptor): SkyResponseCookie {
    return {
      name: cookie.name,
      value: cookie.value,
      maxAge: cookie.maxAge,
      path: cookie.path,
      domain: cookie.domain,
      httpOnly: cookie.httpOnly,
      secure: cookie.secure,
      sameSite: cookie.sameSite,
    };
  }

  /**
   * Build an error SkyResponse.
   */
  private errorResponse(
    status: number,
    code: string,
    message: string
  ): SkyResponse {
    const body = JSON.stringify({ code, message });

    return {
      status,
      body: new TextEncoder().encode(body),
      headers: {},
      cookies: [],
    };
  }

  /**
   * Map handler errors to appropriate responses.
   */
  private handleError(err: unknown, log: any): SkyResponse {
    if (err instanceof HttpError) {
      log.warn(
        { status: err.status, message: err.message },
        "handler returned error"
      );
      return this.errorResponse(err.status, "handler_error", err.message);
    }

    log.error({ err }, "unhandled error in handler");
    return this.errorResponse(
      500,
      "internal_error",
      "An unexpected error occurred"
    );
  }
}
