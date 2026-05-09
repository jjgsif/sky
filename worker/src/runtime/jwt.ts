import { SignJWT, jwtVerify, type JWTPayload } from "jose";

// Encode once per worker process — safe because SKY_AUTH_SECRET is injected
// at spawn time and never changes for the lifetime of the process.
const secret = new TextEncoder().encode(process.env["SKY_AUTH_SECRET"] ?? "");

/**
 * Issue a signed HS256 JWT.
 *
 * Requires `SKY_AUTH_SECRET` to be set in the environment (the gateway
 * injects it automatically from `[auth].jwt_secret` in `sky.toml`).
 *
 * @example
 * // In a login handler:
 * const token = await issueToken({ sub: user.id, role: user.role });
 * return new Response({ token }).status(200);
 *
 * @param claims  Arbitrary claims to embed in the token payload.
 * @param options Optional overrides (e.g. custom `expiresIn`).
 */
export async function issueToken(
    claims: Record<string, unknown>,
    options?: { expiresIn?: string },
): Promise<string> {
    if (!process.env["SKY_AUTH_SECRET"]) {
        throw new Error(
            "issueToken: SKY_AUTH_SECRET is not set. Configure [auth].jwt_secret in sky.toml.",
        );
    }
    return new SignJWT(claims as JWTPayload)
        .setProtectedHeader({ alg: "HS256" })
        .setIssuedAt()
        .setExpirationTime(options?.expiresIn ?? "24h")
        .sign(secret);
}

/**
 * Verify and decode a JWT signed with the gateway's shared secret.
 *
 * Throws if the token is invalid or expired — callers should catch and
 * return an appropriate `HttpError`.
 *
 * @param token  The raw JWT string (without the `Bearer ` prefix).
 */
export async function verifyToken(token: string): Promise<JWTPayload> {
    if (!process.env["SKY_AUTH_SECRET"]) {
        throw new Error(
            "verifyToken: SKY_AUTH_SECRET is not set. Configure [auth].jwt_secret in sky.toml.",
        );
    }
    const { payload } = await jwtVerify(token, secret);
    return payload;
}
