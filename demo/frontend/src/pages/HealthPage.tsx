import { useState, useEffect } from "react";
import type { ApiResult } from "../types";
import { ResultCard } from "../components/ResultCard";

interface HealthResponse {
  status: string;
  uptime: string;
  startedAt: string;
  workerId?: string;
  requestId?: string;
}

export default function HealthPage() {
  const [result, setResult] = useState<ApiResult<HealthResponse>>({ state: "idle" });
  const [autoRefresh, setAutoRefresh] = useState(false);

  const check = async () => {
    setResult({ state: "loading" });
    try {
      const res = await fetch("/health");
      if (!res.ok) {
        const text = await res.text();
        setResult({ state: "error", message: `${res.status}: ${text}` });
        return;
      }
      const data = await res.json() as HealthResponse;
      setResult({ state: "ok", data });
    } catch (e) {
      setResult({ state: "error", message: String(e) });
    }
  };

  useEffect(() => {
    if (!autoRefresh) return;
    check();
    const id = setInterval(check, 2000);
    return () => clearInterval(id);
  }, [autoRefresh]);

  return (
    <div className="page">
      <header className="page-header">
        <h1>GET /health</h1>
        <p className="subtitle">
          Live worker heartbeat. Shows uptime, the worker ID assigned by the
          gateway supervisor, and the per-request trace ID.
        </p>
      </header>

      <section className="card">
        <h2>Status</h2>
        <div className="row">
          <button onClick={check} disabled={result.state === "loading"}>
            {result.state === "loading" ? "Checking…" : "Check now"}
          </button>
          <label className="toggle-label">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
            />
            Auto-refresh every 2s
          </label>
        </div>

        <ResultCard
          result={result}
          render={(d) => (
            <table>
              <tbody>
                <tr>
                  <td>Status</td>
                  <td>
                    <span className={`badge badge--${d.status === "healthy" ? "green" : "red"}`}>
                      {d.status}
                    </span>
                  </td>
                </tr>
                <tr><td>Uptime</td><td>{d.uptime}</td></tr>
                <tr><td>Started</td><td>{new Date(d.startedAt).toLocaleString()}</td></tr>
                {d.workerId && <tr><td>Worker ID</td><td><code>{d.workerId}</code></td></tr>}
                {d.requestId && <tr><td>Request ID</td><td><code>{d.requestId}</code></td></tr>}
              </tbody>
            </table>
          )}
        />
      </section>
    </div>
  );
}
