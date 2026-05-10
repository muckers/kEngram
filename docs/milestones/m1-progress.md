# M1 — Progress

Living checklist tracking M1 implementation. Each phase ends in a runnable, reviewable checkpoint. Items are checked off as they land; the **History** section at the bottom captures dated notes — decisions made in passing, surprises, things deferred. The companion design doc is `m1-capture-and-search.md` in this directory.

## Phase A — Foundation

End state: workspace compiles clean; database schema is loaded.

- [ ] Root `Cargo.toml`: `[workspace]` members + `[workspace.dependencies]` block listing every crate from the CLAUDE.md Stack table, pinned to current stable versions
- [ ] Library crates: `engram-core`, `engram-storage`, `engram-embed`, `engram-mcp` (all empty, all compile)
- [ ] Binary crate: `engram-cli` declaring `[[bin]] name = "engram"`
- [ ] `.gitignore` (Rust `target/`, IDE files, `.env`, `.DS_Store`)
- [ ] `migrations/0001_initial.sql` matching design doc §5
- [ ] `sqlx migrate run` succeeds against the M0 Docker Postgres
- [ ] `cargo build --workspace` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean

## Phase B — Capture vertical slice

End state: an agent can call `capture` over MCP; thought row + embedding row land in the database; soft-fail returns `embedding_status: "pending"` cleanly.

- [ ] `engram-core` domain types: `Thought`, `ThoughtId`, `Scope`, `Source`, `Embedding`, `EmbeddingModel`, `Metadata`
- [ ] `engram-core` `Embedder` trait
- [ ] `engram-embed` `OpenAICompatibleEmbedder` (covers Ollama / TEI / OpenAI / Voyage by config)
- [ ] `engram-embed` `FakeEmbedder` (deterministic; for sqlx-tests with no Ollama dependency)
- [ ] `engram-storage` repository functions: insert thought, insert embedding, fetch thought by id
- [ ] `engram-mcp` `capture` tool descriptor + handler (using rmcp)
- [ ] `engram-cli` `serve` subcommand: axum + rmcp transport on `127.0.0.1:<port>`
- [ ] `figment` config loader: `~/.config/engram/engram.toml` + `ENGRAM_*` env overrides + `--config <path>` override
- [ ] `tracing` initialization: structured output to stderr
- [ ] `sqlx::test`: `capture` with `FakeEmbedder` writes both rows, returns `embedding_status: "indexed"`
- [ ] `sqlx::test`: `capture` with a failing `FakeEmbedder` returns `embedding_status: "pending"`; thought row exists; embedding row absent; WARN logged

## Phase C — Search vertical slice

End state: capture → search end-to-end via MCP. Hybrid retrieval (vector ∪ trigram, RRF) returns ranked results. Trigram-only fallback works when the embedder is down.

- [ ] `engram-storage` vector kNN query against `embeddings_bge_m3_hnsw`
- [ ] `engram-storage` trigram similarity query against `thoughts_content_trgm_idx`
- [ ] `engram-storage` recent-by-scope query against `thoughts_scope_recent_idx`
- [ ] `engram-core` RRF fusion (`k = 60` default; configurable) + post-fusion recency boost
- [ ] `engram-mcp` tools: `search_thoughts`, `recent_thoughts`, `get_thought`
- [ ] Soft-fail on embedder unavailable: `search_thoughts` returns `vector_search_available: false` with trigram-only results
- [ ] `sqlx::test`: full hybrid search round-trip with `FakeEmbedder`
- [ ] `sqlx::test`: search with embedder unavailable returns degraded results plus the flag
- [ ] `sqlx::test`: `recent_thoughts` orders by `created_at DESC`
- [ ] `sqlx::test`: `get_thought` returns full row with `embedding_status` in provenance

## Phase D — Hardening

End state: M1 success criteria from `m1-capture-and-search.md` met.

- [ ] `engram embed-backfill [--scope <s>] [--limit <n>]` subcommand: finds thoughts missing an embedding (LEFT JOIN, IS NULL), embeds them inline
- [ ] `sqlx::test`: backfill finds and embeds previously-pending thoughts
- [ ] `engram migrate` subcommand (wraps sqlx migration runner)
- [ ] `cargo test --workspace --features integration` against live Ollama: real capture → embed → search round-trip
- [ ] MCP smoke test: Claude Code (or `mcp-inspector`) calls all four tools against `engram serve` successfully
- [ ] README quick-start for the operator (or fold into `DEVELOPMENT.md`)
- [ ] Operator dogfood begins (informal; reported in History)

## History

Dated notes appended as items land. Format: `YYYY-MM-DD — <one-line summary>`. Multi-line entries fine for decisions that need explanation.

<!-- Most recent entry first. -->

- (no entries yet)
