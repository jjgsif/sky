import { useState } from "react";
import type { ApiResult } from "../types";
import { ResultCard } from "../components/ResultCard";

interface HelloResponse {
  message: string;
}

export default function HelloPage() {
  const [name, setName] = useState("World");
  const [result, setResult] = useState<ApiResult<HelloResponse>>({ state: "idle" });

  const send = async () => {
    setResult({ state: "loading" });
    try {
      const res = await fetch("/hello", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),
      });
      if (!res.ok) {
        const text = await res.text();
        setResult({ state: "error", message: `${res.status}: ${text}` });
        return;
      }
      const data = await res.json() as HelloResponse;
      setResult({ state: "ok", data });
    } catch (e) {
      setResult({ state: "error", message: String(e) });
    }
  };

  return (
    <div className="page">
      <header className="page-header">
        <h1>POST /hello</h1>
        <p className="subtitle">
          Sends a JSON body to the Sky gateway, which forwards it to the Bun worker
          via the Sky framing protocol. The worker returns a typed greeting.
        </p>
      </header>

      <section className="card">
        <h2>Request</h2>
        <p className="hint">Enter your name and hit Send.</p>
        <div className="row">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Your name"
            onKeyDown={(e) => e.key === "Enter" && send()}
          />
          <button onClick={send} disabled={result.state === "loading"}>
            {result.state === "loading" ? "Sending…" : "Send"}
          </button>
        </div>
        <ResultCard result={result} render={(d) => <code>{d.message}</code>} />
      </section>

      <section className="card">
        <h2>Schema</h2>
        <p className="hint">
          The gateway validates the request body against the JSON Schema emitted by{" "}
          <code>@ZodBody</code> before forwarding to the worker.
        </p>
        <pre className="code-block">{`POST /hello
Content-Type: application/json

{ "name": "World" }

→ 200 { "message": "Hello World" }`}</pre>
      </section>
    </div>
  );
}
