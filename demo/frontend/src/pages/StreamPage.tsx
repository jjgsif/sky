import { useState, useRef } from "react";

type StreamMode = "events" | "fibonacci";

interface StreamItem {
  raw: string;
  parsed: Record<string, unknown>;
}

export default function StreamPage() {
  const [mode, setMode] = useState<StreamMode>("events");
  const [count, setCount] = useState("10");
  const [items, setItems] = useState<StreamItem[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  const start = async () => {
    if (streaming) {
      abortRef.current?.abort();
      return;
    }

    setItems([]);
    setElapsed(null);
    setStreaming(true);
    const controller = new AbortController();
    abortRef.current = controller;
    const t0 = performance.now();

    const param = mode === "events" ? "count" : "limit";
    const url = `/stream/${mode}?${param}=${encodeURIComponent(count)}`;

    try {
      const res = await fetch(url, { signal: controller.signal });
      if (!res.ok) throw new Error(`${res.status}`);
      const reader = res.body!.getReader();
      const decoder = new TextDecoder();
      let buf = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        const lines = buf.split("\n");
        buf = lines.pop() ?? "";
        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed) continue;
          try {
            const parsed = JSON.parse(trimmed) as Record<string, unknown>;
            setItems((prev) => {
              const next = [...prev, { raw: trimmed, parsed }];
              requestAnimationFrame(() => {
                if (logRef.current) {
                  logRef.current.scrollTop = logRef.current.scrollHeight;
                }
              });
              return next;
            });
          } catch {
            // skip malformed line
          }
        }
      }
    } catch (e) {
      if ((e as Error).name !== "AbortError") {
        console.error("Stream error:", e);
      }
    } finally {
      setStreaming(false);
      setElapsed(Math.round(performance.now() - t0));
    }
  };

  const modeEndpoint = mode === "events"
    ? `/stream/events?count=${count}`
    : `/stream/fibonacci?limit=${count}`;

  return (
    <div className="page">
      <header className="page-header">
        <h1>Streaming</h1>
        <p className="subtitle">
          HTTP chunked transfer via <code>streaming: true</code> handlers. The gateway
          pipes RESPONSE_CHUNK frames directly to the client as they arrive.
        </p>
      </header>

      <section className="card">
        <h2>Configure</h2>
        <div className="row" style={{ flexWrap: "wrap", gap: "0.75rem" }}>
          <div className="tab-group">
            <button
              className={`tab${mode === "events" ? " tab--active" : ""}`}
              onClick={() => { setMode("events"); setItems([]); }}
            >
              Events
            </button>
            <button
              className={`tab${mode === "fibonacci" ? " tab--active" : ""}`}
              onClick={() => { setMode("fibonacci"); setItems([]); }}
            >
              Fibonacci
            </button>
          </div>
          <div className="row" style={{ marginBottom: 0 }}>
            <label className="field-label">
              {mode === "events" ? "Count" : "Limit"}
            </label>
            <input
              type="number"
              value={count}
              min={1}
              max={100}
              style={{ width: "80px" }}
              onChange={(e) => setCount(e.target.value)}
            />
          </div>
          <button onClick={start} className={streaming ? "btn-danger" : ""}>
            {streaming ? "Stop" : "Start stream"}
          </button>
        </div>
        <p className="hint" style={{ marginTop: "0.5rem" }}>
          <code>GET {modeEndpoint}</code>
        </p>
      </section>

      <section className="card">
        <div className="stream-header">
          <h2>Output</h2>
          {items.length > 0 && (
            <span className="stream-meta">
              {items.length} item{items.length !== 1 ? "s" : ""}
              {elapsed !== null && ` · ${elapsed}ms`}
            </span>
          )}
        </div>

        <div ref={logRef} className="stream-log">
          {items.length === 0 && !streaming && (
            <span className="stream-empty">No output yet — click Start stream</span>
          )}
          {streaming && items.length === 0 && (
            <span className="stream-empty">Streaming…</span>
          )}
          {items.map((item, i) => (
            <div key={i} className="stream-row">
              <span className="stream-index">{i + 1}</span>
              <StreamItemView item={item} mode={mode} />
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

function StreamItemView({ item, mode }: { item: StreamItem; mode: StreamMode }) {
  if (mode === "events") {
    const { seq, time, message } = item.parsed as { seq: number; time: string; message: string };
    return (
      <span className="stream-item">
        <span className="stream-seq">#{seq}</span>
        <span className="stream-msg">{message}</span>
        <span className="stream-time">{new Date(time).toLocaleTimeString("en", { hour12: false })}</span>
      </span>
    );
  }
  const { index, value } = item.parsed as { index: number; value: number };
  return (
    <span className="stream-item">
      <span className="stream-seq">F({index})</span>
      <span className="stream-msg">{value.toLocaleString()}</span>
    </span>
  );
}
