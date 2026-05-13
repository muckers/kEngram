# Engram

Self-hosted, MCP-native memory service for AI agents. Single Rust binary; Postgres + pgvector backing store; vendor-neutral via an OpenAI-compatible embedding endpoint.

The full design lives in [`docs/engram-design-v0.md`](docs/engram-design-v0.md), with per-milestone scope in [`docs/milestones/`](docs/milestones/). For first-time setup see [`DEVELOPMENT.md`](DEVELOPMENT.md).

## Status

Built in five capability milestones (M1 → M5), preceded by an environment milestone (M0). **M1 is implemented**: capture + hybrid search + four MCP tools.

| Milestone | Status | What it adds |
|---|---|---|
| [M0 — dev environment](docs/milestones/m0-dev-environment.md) | ✅ | Docker Postgres + Ollama dev path |
| [M1 — capture & search](docs/milestones/m1-capture-and-search.md) | ✅ | `capture`, `search_thoughts`, `recent_thoughts`, `get_thought` over MCP |
| [M2 — facts pipeline](docs/milestones/m2-facts-pipeline.md) | ⏳ | Extractor, worker process, async embedding, facts table populated |
| [M3 — search quality](docs/milestones/m3-search-quality.md) | ⏳ | Cross-encoder reranker |
| [M4 — artifacts](docs/milestones/m4-artifacts.md) | ⏳ | Long-form document ingestion |
| [M5 — operational maturity](docs/milestones/m5-operational-maturity.md) | ⏳ | Metrics, Tier 2 auth, eval suite, backups |

Progress against M1 is tracked in [`docs/milestones/m1-progress.md`](docs/milestones/m1-progress.md).

## Quick start

```bash
# 1. Bring up the dev environment (M0 — see DEVELOPMENT.md)
docker compose up -d postgres
ollama pull bge-m3

# 2. Apply migrations
DATABASE_URL='postgres://engram:engram@localhost:5432/engram' \
  cargo run --bin engram -- migrate

# 3. Run the MCP server
DATABASE_URL='postgres://engram:engram@localhost:5432/engram' \
  cargo run --bin engram -- serve
```

The server binds `127.0.0.1:8080` by default and exposes a streamable-HTTP MCP endpoint at `/mcp` (per the current MCP spec, via rmcp's `StreamableHttpService`). Point an MCP-capable client (Claude Code, Claude Desktop, `mcp-inspector`) at `http://127.0.0.1:8080/mcp` to use the four tools.

## MCP surface (M1)

| Tool | What it does |
|---|---|
| `capture` | Record a thought. Returns `thought_id` + `embedding_status` (`indexed` or `pending`). |
| `search_thoughts` | Hybrid retrieval (vector kNN ∪ trigram, fused by RRF, recency-boosted). Gracefully degrades to trigram-only when the embedder is unreachable; result includes `vector_search_available: bool`. |
| `recent_thoughts` | Browse by recency in a (optional) scope. |
| `get_thought` | Full thought + provenance (embedding status, embedded-at timestamp, linked facts in M2). |

See the [M1 milestone doc](docs/milestones/m1-capture-and-search.md) for argument shapes, defaults, and the soft-fail design.

## Repo layout

```
crates/
├── engram-core/      # domain types, Embedder trait, RRF + recency_boost (pure)
├── engram-storage/   # sqlx queries, migrations, repository functions
├── engram-embed/     # Embedder impls: OpenAICompatibleEmbedder, FakeEmbedder
├── engram-mcp/       # capture/search/get/recent orchestrators + rmcp tool wiring
└── engram-cli/       # binary; serve/migrate/embed-backfill subcommands
migrations/           # sqlx migrations (numbered)
docs/                 # design doc + per-milestone scope/progress
```

`engram-extract` joins at M2 when the facts pipeline lands.

## License

TBD — not currently published.
