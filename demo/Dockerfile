# Build context: repo root (sky/)
# docker build -f demo/Dockerfile ..

# ── Stage 1: Compile the Rust gateway ────────────────────────────────────────
FROM rust:latest AS gateway-builder

WORKDIR /build

# Build-time system deps required by crate build scripts (openssl-sys, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Layer-cache the dependency fetch separately from source compilation.
# Cargo needs the full workspace manifest to resolve the dependency graph.
# src/ is the root workspace package (sky); without it Cargo rejects the manifest.
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY crates/ ./crates/

RUN cargo build --release -p sky-gateway

# ── Stage 2: Build the Sky manifest ──────────────────────────────────────────
FROM oven/bun:1 AS manifest-builder

WORKDIR /build

# Install worker package dependencies first (demo depends on sky via file:)
COPY worker/ ./worker/
RUN cd worker && bun install

# Install demo dependencies and generate sky-manifest.json
COPY demo/ ./demo/
WORKDIR /build/demo
RUN bun install
RUN bun run build

# ── Stage 3: Runtime image ────────────────────────────────────────────────────
FROM oven/bun:1

WORKDIR /app

# Gateway binary
COPY --from=gateway-builder /build/target/release/sky-gateway ./sky-gateway

# Worker: copy source and install deps in the final location (/app/worker).
# Must run before demo install so the file: symlink resolves to /app/worker.
COPY worker/ ./worker/
RUN cd worker && bun install

# Demo: copy source (node_modules excluded by .dockerignore) and install deps.
# bun install here creates node_modules/sky -> ../worker which resolves to /app/worker.
COPY demo/ ./demo/
WORKDIR /app/demo
RUN bun install

# Overlay the generated manifest produced by the manifest-builder stage.
COPY --from=manifest-builder /build/demo/sky-manifest.json ./sky-manifest.json

# Run from the demo directory so all relative paths in sky.docker.toml
# (worker_script, manifest_path) resolve correctly.
WORKDIR /app/demo

EXPOSE 8080

CMD ["/app/sky-gateway", "--config", "sky.docker.toml"]
