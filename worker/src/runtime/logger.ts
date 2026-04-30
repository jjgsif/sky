import pino, { type LoggerOptions, type Logger as PinoLogger } from "pino";

const isDev = process.env.NODE_ENV !== "production";
const isBuild = process.env.SKY_BUILD === "1";

let _logger: PinoLogger | undefined;

function createLogger(): PinoLogger {
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

  return pino(options);
}

export const logger: PinoLogger = new Proxy({} as PinoLogger, {
  get(_, prop) {
    if (!_logger) {
      if (isBuild) {
        // During sky build, use a no-op logger
        _logger = pino({ level: "silent" });
      } else {
        _logger = createLogger();
      }
    }
    return (_logger as any)[prop];
  },
});

export type Logger = PinoLogger;