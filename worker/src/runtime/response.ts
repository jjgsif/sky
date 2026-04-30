/**
 * Response
 *
 * Framework-provided response wrapper for Sky handlers. Handlers
 * can return a plain object (Sky wraps it with the default status
 * from the HTTP method) or construct a Response explicitly to
 * control status, headers, and cookies.
 *
 * @example
 * // Simple — returns 200 (GET) or 201 (POST) automatically
 * async getUser(id: string) {
 *   return { id, name: "Alice" };
 * }
 *
 * // Explicit control
 * async createUser(body: CreateUserRequest) {
 *   const user = await db.insert(body);
 *   return new Response(user)
 *     .status(201)
 *     .header("x-created-by", "sky")
 *     .cookie("session", token, { httpOnly: true, secure: true, maxAge: 3600 });
 * }
 *
 * // Clear a cookie
 * async logout() {
 *   return new Response(null)
 *     .status(204)
 *     .clearCookie("session");
 * }
 */

export interface CookieOptions {
  maxAge?: number;
  path?: string;
  domain?: string;
  httpOnly?: boolean;
  secure?: boolean;
  sameSite?: "Strict" | "Lax" | "None";
}

export interface SetCookieDescriptor {
  name: string;
  value: string;
  maxAge?: number;
  path?: string;
  domain?: string;
  httpOnly: boolean;
  secure: boolean;
  sameSite?: string;
}

export class Response<T = any> {
  private _body: T;
  private _status: number = 0; // 0 = use default from handler
  private _headers: Map<string, string> = new Map();
  private _cookies: SetCookieDescriptor[] = [];

  constructor(body: T) {
    this._body = body;
  }

  /**
   * Set the HTTP status code.
   * Overrides the default status from the handler's HTTP method.
   */
  status(code: number): this {
    this._status = code;
    return this;
  }

  /**
   * Set a response header.
   */
  header(name: string, value: string): this {
    this._headers.set(name, value);
    return this;
  }

  /**
   * Set a cookie on the response.
   */
  cookie(name: string, value: string, options: CookieOptions = {}): this {
    this._cookies.push({
      name,
      value,
      maxAge: options.maxAge,
      path: options.path,
      domain: options.domain,
      httpOnly: options.httpOnly ?? false,
      secure: options.secure ?? false,
      sameSite: options.sameSite,
    });
    return this;
  }

  /**
   * Clear a cookie by setting its value to empty and maxAge to 0.
   */
  clearCookie(name: string, options: Pick<CookieOptions, "path" | "domain"> = {}): this {
    this._cookies.push({
      name,
      value: "",
      maxAge: 0,
      path: options.path ?? "/",
      domain: options.domain,
      httpOnly: false,
      secure: false,
      sameSite: undefined,
    });
    return this;
  }

  /** Get the response body. */
  getBody(): T {
    return this._body;
  }

  /** Get the status code (0 means use default). */
  getStatus(): number {
    return this._status;
  }

  /** Get all response headers as a plain object. */
  getHeaders(): Record<string, string> {
    return Object.fromEntries(this._headers);
  }

  /** Get all cookie descriptors. */
  getCookies(): SetCookieDescriptor[] {
    return this._cookies;
  }
}
