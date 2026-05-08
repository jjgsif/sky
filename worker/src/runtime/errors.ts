/**
 * Throw from a handler to send a specific HTTP status code to the client.
 *
 * @example
 * if (!user) throw new HttpError(404, "User not found");
 * if (!authorized) throw new HttpError(403, "Forbidden");
 */
export class HttpError extends Error {
    constructor(
        public readonly status: number,
        message: string,
    ) {
        super(message);
        this.name = "HttpError";
    }
}
