import { Service, Handler, StreamedBody, Group, Context, Param, type ExtractContext } from "sky-framework/decorators";
import { parseMultipart, HttpError } from "sky-framework/runtime";
import fs from "node:fs";
import path from "node:path";

const ATTACHMENTS = "/tmp/attachments";

const uploadExtract = {
    stream: StreamedBody(),
    ctx: Context(),
} as const;

const downloadExtract = {
    filename: Param("filename"),
} as const;

@Service({ lifetime: "scoped" })
@Group({ prefix: "/api/files" })
export class FileUploadService {

    @Handler({ method: "POST", path: "/upload", extract: uploadExtract, validate: false })
    async upload({ stream, ctx }: ExtractContext<typeof uploadExtract>) {
        if (!fs.existsSync(ATTACHMENTS)) {
            fs.mkdirSync(ATTACHMENTS, { recursive: true });
        }

        const contentType = ctx.headers["content-type"] ?? "";
        const saved: Array<{ filename: string; field: string }> = [];

        for await (const part of parseMultipart(stream, contentType)) {
            const dest = path.join(ATTACHMENTS, part.filename ?? part.field);
            await part.saveTo(dest);
            saved.push({ filename: part.filename ?? part.field, field: part.field });
        }

        return {
            status: 201,
            body: { files: saved, count: saved.length },
        };
    }

    @Handler({ method: "GET", path: "/download/:filename", extract: downloadExtract, streaming: true, validate: false })
    async download({ filename }: ExtractContext<typeof downloadExtract>) {
        const filePath = path.join(ATTACHMENTS, path.basename(filename));

        if (!fs.existsSync(filePath)) {
            throw new HttpError(404, `File not found: ${filename}`);
        }

        const file = Bun.file(filePath);

        return {
            status: 200,
            headers: {
                "content-type": file.type || "application/octet-stream",
                "content-disposition": `attachment; filename="${path.basename(filename)}"`,
                "content-length": String(file.size),
            },
            body: streamFile(file),
        };
    }
}

async function* streamFile(file: ReturnType<typeof Bun.file>): AsyncGenerator<Uint8Array> {
    const reader = file.stream().getReader();
    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) break;
            yield value;
        }
    } finally {
        reader.releaseLock();
    }
}
