# syntax=docker/dockerfile:1
# Base image: pre-compiled sky-gateway binary + Bun runtime.
# Users extend this image and copy their worker files.
#
# Example:
#   FROM ghcr.io/sky-framework/sky:1.0.0
#   COPY worker/ ./worker/
#   COPY sky.toml sky-manifest.json ./
#   CMD ["sky-gateway", "--config", "sky.toml"]
FROM oven/bun:1-alpine

ARG TARGETARCH=amd64
COPY dist/${TARGETARCH}/sky-gateway /usr/local/bin/sky-gateway
RUN chmod +x /usr/local/bin/sky-gateway

WORKDIR /app
EXPOSE 8080
CMD ["sky-gateway"]
