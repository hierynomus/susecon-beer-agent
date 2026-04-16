# beer-mcp

An [MCP](https://modelcontextprotocol.io/) server that accepts beer orders and dispatches a human for delivery. Built with Rust for SUSECON.

## What it does

Exposes a single MCP tool — `order_beer` — that an AI agent can call to place a beer order. The server simulates processing, then responds with a confirmation message. A human operative handles the actual delivery.

## Running locally

```sh
cargo run
```

The server listens on port `3000` by default. Override with the `PORT` environment variable.

- **MCP endpoint:** `http://localhost:3000/mcp`
- **Health check:** `http://localhost:3000/health`

Logging level is controlled via `RUST_LOG` (e.g. `RUST_LOG=debug`).

## Docker

The Dockerfile expects pre-compiled binaries to be provided via a `binaries` build context (produced by `cross`):

```sh
cross build --release --target aarch64-unknown-linux-musl
docker buildx build \
  --build-context binaries=./target \
  -t beer-mcp .
```

## Helm

A Helm chart is included under `charts/beer-mcp/`. Minimal install:

```sh
helm install beer-mcp ./charts/beer-mcp \
  --set ingress.host=beer-mcp.example.com
```

See [`charts/beer-mcp/values.yaml`](charts/beer-mcp/values.yaml) for all available options including TLS and cert-manager integration.
