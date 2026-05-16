import { useRef, useState } from "react";
import * as React from "react";

interface UploadedFile {
  filename: string;
  field: string;
}

interface UploadResponse {
  files: UploadedFile[];
  count: number;
}

type UploadState =
  | { state: "idle" }
  | { state: "uploading" }
  | { state: "done"; data: UploadResponse }
  | { state: "error"; message: string };

export default function UploadPage() {
  const [files, setFiles] = useState<File[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const [uploadState, setUploadState] = useState<UploadState>({ state: "idle" });
  const inputRef = useRef<HTMLInputElement>(null);

  const addFiles = (incoming: FileList | null) => {
    if (!incoming) return;
    setFiles((prev) => {
      const next = [...prev];
      for (const f of incoming) {
        if (!next.some((x) => x.name === f.name && x.size === f.size)) {
          next.push(f);
        }
      }
      return next;
    });
  };

  const removeFile = (index: number) => {
    setFiles((prev) => prev.filter((_, i) => i !== index));
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    addFiles(e.dataTransfer.files);
  };

  const upload = async () => {
    if (files.length === 0) return;
    setUploadState({ state: "uploading" });
    try {
      const form = new FormData();
      for (const f of files) form.append("file", f, f.name);
      const res = await fetch("/api/files/upload", { method: "POST", body: form });
      if (!res.ok) {
        const text = await res.text();
        setUploadState({ state: "error", message: `${res.status}: ${text}` });
        return;
      }
      const data = await res.json() as UploadResponse;
      setUploadState({ state: "done", data });
      setFiles([]);
    } catch (e) {
      setUploadState({ state: "error", message: String(e) });
    }
  };

  const reset = () => {
    setFiles([]);
    setUploadState({ state: "idle" });
  };

  const uploading = uploadState.state === "uploading";

  return (
    <div className="page">
      <header className="page-header">
        <h1>POST /api/files/upload</h1>
        <p className="subtitle">
          Multipart file upload handled by the Sky gateway via StreamedBody. Files are
          saved server-side and available for download.
        </p>
      </header>

      <section className="card">
        <h2>Select files</h2>
        <p className="hint">Drag and drop files or click to browse.</p>

        <div
          className={`drop-zone${dragOver ? " drop-zone--over" : ""}`}
          onClick={() => inputRef.current?.click()}
          onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
        >
          <span className="drop-zone-icon">+</span>
          <span className="drop-zone-label">
            {dragOver ? "Drop to add" : "Click or drag files here"}
          </span>
          <input
            ref={inputRef}
            type="file"
            multiple
            style={{ display: "none" }}
            onChange={(e) => addFiles(e.target.files)}
          />
        </div>

        {files.length > 0 && (
          <ul className="file-list">
            {files.map((f, i) => (
              <li key={i} className="file-row">
                <span className="file-name">{f.name}</span>
                <span className="file-size">{formatBytes(f.size)}</span>
                <button
                  className="btn-danger file-remove"
                  onClick={() => removeFile(i)}
                  disabled={uploading}
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}

        <div className="row" style={{ marginTop: "1rem" }}>
          <button onClick={upload} disabled={uploading || files.length === 0}>
            {uploading ? "Uploading…" : `Upload ${files.length > 0 ? files.length : ""} file${files.length !== 1 ? "s" : ""}`}
          </button>
          {(files.length > 0 || uploadState.state !== "idle") && (
            <button className="btn-danger" onClick={reset} disabled={uploading}>
              Reset
            </button>
          )}
        </div>

        {uploadState.state === "uploading" && (
          <p className="result loading">Uploading…</p>
        )}
        {uploadState.state === "error" && (
          <p className="result error">Error: {uploadState.message}</p>
        )}
        {uploadState.state === "done" && (
          <div className="result ok">
            Uploaded {uploadState.data.count} file{uploadState.data.count !== 1 ? "s" : ""} successfully.
          </div>
        )}
      </section>

      {uploadState.state === "done" && uploadState.data.files.length > 0 && (
        <section className="card">
          <h2>Uploaded files</h2>
          <p className="hint">Click Download to retrieve a file via the streaming download endpoint.</p>
          <ul className="file-list">
            {uploadState.data.files.map((f, i) => (
              <li key={i} className="file-row">
                <span className="file-name">{f.filename || f.field}</span>
                <a
                  className="download-link"
                  href={`/api/files/download/${encodeURIComponent(f.filename || f.field)}`}
                  download={f.filename || f.field}
                >
                  Download
                </a>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="card">
        <h2>Schema</h2>
        <pre className="code-block">{`POST /api/files/upload
Content-Type: multipart/form-data; boundary=...

[binary parts]

→ 201 { "files": [{ "filename": "foo.png", "field": "file" }], "count": 1 }

GET /api/files/download/:filename
→ 200 (streaming, Content-Disposition: attachment)`}</pre>
      </section>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
