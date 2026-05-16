import { globalMiddleware } from "sky-framework/emitter";
import { cors } from "sky-framework/runtime";

globalMiddleware([
    cors({
        origins: ["*"],
        credentials: false,
        maxAge: 86400,
        allowHeaders: ["content-type", "authorization"],
    }),
]);
