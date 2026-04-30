import type { GroupOptions, SkyClassMetadata } from "./types";

const Group = ({prefix, middleware}: GroupOptions) => (value: Function, context: ClassDecoratorContext) => {
    (context.metadata as SkyClassMetadata).group = {
        prefix,
        middleware: middleware ?? []
    }
}

export { Group };