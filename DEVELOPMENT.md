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

### 4. Run migrations (once M1 lands)

```bash
sqlx migrate run
```

The migration creates the extensions in the `engram` database and ships the schema described in `docs/engram-design-v0.md` §5.

### 5. Build and test

```bash
cargo build --workspace
cargo test --workspace
```

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
```

## Port conflicts

If something else already binds `5432`, edit `docker-compose.yml` to map a different host port (e.g. `"5433:5432"`) and update `DATABASE_URL` accordingly.

## Production note

In production, Postgres runs as a systemd-managed service (not Docker), and the embedder is a TEI sidecar (also systemd-managed) rather than Ollama. Both deployment shapes are described in `docs/engram-design-v0.md` §11. The dev setup here exists for ergonomics — the production setup is operator-managed and out of scope for this file.
