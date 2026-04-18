import pino, { type LoggerOptions } from "pino";

/**
 * Structured JSON logger for the Sky worker.
 *
 * In development, logs are pretty-printed to the console via pino-pretty.
 * In production, raw JSON is emitted for log aggregators to parse.
 *
 * Child loggers should be used to bind request-scoped fields like
 * requestId, so every log line within a request's handling carries
 * its identifier without explicit field passing.
 */

const isDev = process.env.NODE_ENV !== "production";

const options: LoggerOptions = {
    level: process.env.LOG_LEVEL ?? "info",
};

if (isDev) {
    options.transport = {
        target: "pino-pretty",
        options: {
            colorize: true,
            translateTime: "HH:MM:ss.l",
            ignore: "pid,hostname",
        },
    };
}

export const logger = pino(options);

export type Logger = typeof logger;