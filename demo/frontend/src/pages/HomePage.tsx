import { Link } from "react-router-dom";

const features = [
  {
    to: "/hello",
    title: "POST /hello",
    desc: "Send a greeting through the Sky gateway to a Bun worker and get a typed response back.",
    badge: "Unary",
    color: "#6366f1",
  },
  {
    to: "/health",
    title: "GET /health",
    desc: "Smoke-test the gateway and worker. See uptime, worker ID, and request ID.",
    badge: "Health",
    color: "#22c55e",
  },
  {
    to: "/stream",
    title: "GET /stream/*",
    desc: "Live NDJSON streams over HTTP chunked transfer. Events and Fibonacci sequences.",
    badge: "Streaming",
    color: "#f59e0b",
  },
  {
    to: "/users",
    title: "/users CRUD",
    desc: "Full create / read / update / delete with Zod body validation via the gateway.",
    badge: "CRUD",
    color: "#a78bfa",
  },
];

export default function HomePage() {
  return (
    <div className="page">
      <header className="page-header">
        <h1>Sky Framework Demo</h1>
        <p className="subtitle">
          TypeScript services · Rust gateway · Bun workers — all wired through
          the Sky framing protocol.
        </p>
      </header>

      <div className="home-grid">
        {features.map((f) => (
          <Link key={f.to} to={f.to} className="home-card">
            <div className="home-card-badge" style={{ color: f.color, borderColor: f.color + "44", background: f.color + "11" }}>
              {f.badge}
            </div>
            <h2 className="home-card-title">{f.title}</h2>
            <p className="home-card-desc">{f.desc}</p>
          </Link>
        ))}
      </div>

      <section className="card info-card">
        <h2>How it works</h2>
        <p className="hint" style={{ marginBottom: "0.6rem" }}>
          Each request flows: <strong>Browser → Sky gateway (Rust) → Sky framing protocol → Bun worker (TS)</strong>.
          The gateway reads <code>sky-manifest.json</code> to build its routing table and validate request bodies.
          The worker uses decorators to declare handlers and DI to resolve services.
        </p>
        <pre className="info-proto">{`Browser  ──[HTTP]──▶  Gateway (Rust)
                           │
                    [UDS + MsgPack]
                           │
                      Worker (Bun)`}</pre>
      </section>
    </div>
  );
}
