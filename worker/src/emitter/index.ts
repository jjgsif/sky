// src/index.ts
export { assembleManifest } from "./assembler";
export {ProtoEmitter, type Manifest} from "./proto-emitter";
export type { 
    ManifestOutput,
    ManifestService,
    ManifestHandler,
    ManifestExtract,
    ManifestDependency,
    ManifestMiddleware,
    ManifestGroup,
} from "./assembler";
export * from "./registry";
export * from "./schema";
export * from "./walker";