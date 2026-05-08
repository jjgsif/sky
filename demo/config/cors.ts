import { globalMiddleware } from "sky/emitter";
import { cors } from "sky/runtime";

globalMiddleware([
    cors({
        origins: ["*"],
        credentials: false,
        maxAge: 86400,
        allowHeaders: ["content-type", "authorization"],
    }),
]);
