# Development setup

Quick start for working on Engram on macOS. Assumes Docker, Rust (`rustc` 1.95+), `sqlx-cli`, and Ollama are already installed.

## First-time setup

### 1. Start Postgres in Docker

```bash
docker compose up -d postgres
```

This launches `pgvector/pgvector:pg16` with the `vector`, `pg_trgm`, and `pgcrypto` extensions available. The container is named `engram-postgres`. Data lives in a named Docker volume (`engram-pg-data`) and survives `docker compose down`.

Wait for it to be healthy:

```bash
docker compose ps postgres
# STATUS should show "healthy"
```

### 2. Set the database URL

```bash
export DATABASE_URL="postgres://engram:engram@localhost:5432/engram"
```

`sqlx` and the engram binary both read this. Add it to your shell rc if you don't want to set it every session.

### 3. Pull the embedding model in Ollama

```bash
ollama pull bge-m3
```

Engram's dev-mode embedder talks to Ollama's OpenAI-compatible endpoint (`http://localhost:11434/v1/embeddings`) and uses `bge-m3` for 1024-dim embeddings.

Verify:

```bash
curl http://localhost:11434/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"bge-m3","input":"hello"}' | jq '.data[0].embedding | length'
# expect: 1024
```

### 4. Run migrations

```bash
cargo run --bin engram -- migrate
# or, equivalently
sqlx migrate run
```

The migration creates the three required extensions in the `engram` database and ships the schema described in `docs/engram-design-v0.md` §5.

### 5. Build, test, run

```bash
cargo build --workspace
cargo test --workspace                       # 106 tests (unit + sqlx::test)
cargo test --workspace --features integration   # adds a live-Ollama round-trip test

cargo run --bin engram -- serve              # starts the MCP server on 127.0.0.1:8080
```

Point an MCP-capable client (Claude Code, Claude Desktop, `mcp-inspector`) at `http://127.0.0.1:8080/mcp` (streamable-HTTP transport, per the current MCP spec).

`sqlx::query!` macros and the `sqlx::test` attribute both require `DATABASE_URL` to be set at *build time*, not just at runtime. The `.env` file at the workspace root is read by `sqlx-cli` but NOT by `cargo build` — set `DATABASE_URL` in your shell or pass it inline: `DATABASE_URL=... cargo build`.

## Common operations

```bash
# Stop the database (data persists)
docker compose down

# Stop and wipe the database
docker compose down -v

# Open a psql session in the container
docker exec -it engram-postgres psql -U engram -d engram

# Tail Postgres logs
docker compose logs -f postgres

# Find captures that landed as `embedding_status: "pending"` (e.g. because
# Ollama was down at capture time) and re-embed them inline:
cargo run --bin engram -- embed-backfill --limit 1000
# or restricted to one scope:
cargo run --bin engram -- embed-backfill --scope work --limit 100
```

## Configuration

Defaults live in code. Override via `~/.config/engram/engram.toml`, a `--config <path>` argument, or `ENGRAM_*` env vars (nested via `__`, e.g. `ENGRAM_DATABASE__URL`).

Example `engram.toml`:

```toml
[server]
bind = "127.0.0.1:8080"

[database]
url = "postgres://engram:engram@localhost:5432/engram"
max_connections = 10

[embedder]
provider = "openai-compatible"
endpoint = "http://localhost:11434/v1"   # Ollama for dev; TEI in production
model = "bge-m3"
model_id = "bge-m3:1024"                 # must match an HNSW partial index
dimensions = 1024
timeout_seconds = 5
```

## Port conflicts

If something else already binds `5432`, edit `docker-compose.yml` to map a different host port (e.g. `"5433:5432"`) and update `DATABASE_URL` accordingly.

## Production note

In production, Postgres runs as a systemd-managed service (not Docker), and the embedder is a TEI sidecar (also systemd-managed) rather than Ollama. Both deployment shapes are described in `docs/engram-design-v0.md` §11. The dev setup here exists for ergonomics — the production setup is operator-managed and out of scope for this file.
