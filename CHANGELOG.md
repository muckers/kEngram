# Changelog

All notable changes to kEngram are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1] — 2026-11-11

Post-0.4.0 polish for the human-facing surfaces and the nightly window: the
dotted scope hierarchy now renders as a tree in `kengram stats` and in the
web scope browser (collapsible there), and the nightly tagger systemd unit
stops reporting a false failure every morning. No schema, wire-shape, or
config changes — drop-in upgrade, no migration.

### Changed
- **`kengram stats` Scopes section is now an indented scope tree.** The flat
  most-recently-used scope list is rebuilt in the CLI into a box-drawing tree
  over the dotted hierarchy (alphabetical at every level; ancestors no
  thought sits at directly render as branch-only nodes). The headline count
  is the **whole-subtree rollup**, with the exact scope's own count in parens
  when smaller (`tech 296 (280)`), so rescoped taxonomies read at a glance.
  `--top-scopes N` now keeps the N largest subtrees (was: N most-recently-
  used); pruned siblings collapse to `… (+M scopes, K thoughts)` lines.
  Presentation-only: `list_scopes`, the `corpus_stats` wire shape, and the
  web `/stats` page are unchanged.
- **The web `/scopes` browser is now a collapsible scope tree.** The flat
  full-scope list is replaced by the same dotted-hierarchy view as the CLI
  Scopes tree: alphabetical at every level, branch-only ancestors (`rjf`,
  `project`) rendered as heads, counts are whole-subtree rollups with the
  exact scope's own count in parens when smaller (`tech 296 (280)`). Each
  head collapses/expands via server-rendered `<details>` — expanded by
  default, keyboard-accessible, zero JS. Exact-scope names keep linking to
  `/scope/{name}`; branch heads are inert text. Unlike the CLI the page is
  never truncated. Presentation-only: the `/stats` page, every `/api/*`
  JSON shape, `/scope/{name}`, and storage are unchanged.

### Fixed
- **Nightly tagger window no longer reports a false failure.**
  `contrib/systemd/kengram-tagger-night.service` ran `kengram worker` (which
  never self-exits) under `RuntimeMaxSec=21600`, and systemd always records
  a RuntimeMaxSec-triggered stop as `failed (Result: timeout)` — so every
  window, including successful ones, showed `failed` each morning (observed
  3+ weeks live). `ExecStart` now ends the window itself via an inner
  `timeout --signal=INT` (the same graceful shutdown `KillSignal` maps to),
  normalizing the expected time-reached sentinel to exit 0; `RuntimeMaxSec`
  stays as the outer safety net and genuine failures still redden the unit.
  Docs walkthrough updated.

## [0.4.0] — 2026-11-08

Thought rescoping (M7.2): supervised re-filing of existing thoughts under a new
scope, with an append-only audit trail. Amends the immutability contract
(DESIGN.md §10): `scope` is operational metadata, mutable **only** through the
rescope paths — `content`, `source`, and `created_at` stay immutable.

### Added
- **Thought rescoping (M7.2).** `rescope_thought` MCP tool + `kengram rescope`
  CLI re-file a thought under a new scope; update, audit, and tag-job requeue
  commit in one transaction (locking pre-read; PG16-safe audit). The tool
  requires a one-sentence `reason` and takes an optional free-text `actor`;
  it is idempotent (already at target → `changed: false`), with unknown ids a
  clean error distinct from the no-op. The CLI adds **subtree renames**
  (`--scope <old> --to <new>` — dotted-boundary: `work.foobar` is never
  touched by `work → …`; CLI-only, taxonomy maintenance is an operator
  workflow) and `--dry-run` on both forms, which runs the identical guards and
  plan, prints accurate match + requeue counts, and commits nothing.
- **`scope_changes` audit table (migration `0012`).** Append-only: every move
  records `old_scope` (the pre-update value per row), `new_scope`, `actor`,
  `reason`, `changed_at`. Full history stays SQL-only. Guards shared by CLI
  and MCP — required non-blank reason, target validation, empty-dotted-segment
  rejection (`work.`, `work..x`, `.work`), self-nesting rejection
  (`work → work.archive`) — enforced again inside the storage write engine as
  defense-in-depth.
- **Re-tagging on move.** Rescoped live thoughts re-enqueue their tag job
  (retracted rows and an unconfigured `[tagger]` do not), so tags re-derive
  under the new scope's controlled vocabulary.
- **Last-rescope hint.** `get_thought` / `/api/thoughts/{id}` / web
  thought-detail surface `rescoped_at` + `previous_scope` (the latest
  `scope_changes` hop), mirroring how retraction state renders.

### Changed
- **Contract:** `thoughts.scope` reclassified from claim content to operational
  metadata (DESIGN.md §10 amendment, 2026-11-06). Ad-hoc `UPDATE … SET scope`
  remains forbidden; the sanctioned write paths are the rescope CLI/tool, and
  they keep the audit trail.

## [0.3.0] — 2026-06-30

Read-only web UI (M8) over the existing corpus, plus two retrieval-core fixes.

### Added
- **Read-only web read surface (M8).** A human-facing browser UI served by the
  `kengram` binary, gated behind `[web].enabled` (default false): full-text/hybrid
  search, thought-detail with provenance, an interactive link-graph visualization
  (cytoscape), and a scope browser. Rust SSR (askama) + vanilla JS — no Node/TS
  toolchain. Five read-only `/api/*` endpoints reuse the same `kengram-mcp`
  orchestrators as the MCP tools and serialize through their canonical mappers, so
  `/api/*` JSON is byte-identical to `/mcp`. A Host-header guard mirrors the rmcp
  `allowed_hosts` boundary. No write/mutation routes; `psql` remains the
  write/admin interface and `/mcp` the agent interface. This reverses the v0.1.0
  "no web UI" non-goal.
- **Hierarchical scope search.** A trailing dot (`rjf.`) searches the whole scope
  subtree (`scope_prefix`); an exact scope (`rjf.tech`) stays an exact match.
- **Relevance floor (web).** A Focused/Balanced/Broad selector drops the weak
  vector-kNN tail the cross-encoder scored below a `min_score` floor; no-op when
  the reranker didn't run, so an embedder outage never blanks the page.

### Fixed
- **`search_thoughts` honors `limit > 32`.** The per-leg fetch and rerank
  `candidate_pool` now scale to the requested `limit` instead of being silently
  capped at the default-32 pool. Benefits MCP agents too; no wire-shape change.
- **Reranker chunks oversized batches.** `TeiReranker` now splits rerank batches
  to `[reranker].max_batch` (default 32 = TEI `--max-client-batch-size`) and merges
  with offset, fixing the HTTP-422 that the `limit` fix exposed.

## [0.2.0] — 2026-06-15

### Added
- **Tagger-quality eval harness (M7.1).** A golden-corpus evaluation harness that
  scores tagger output (precision/recall over people/entities/topics/kind) directly
  from the database, with a cross-model sweep (`tagger-sweep.sh`) and a dedicated
  eval-harness reference doc. Plus M7.1.1 unattended-run hardening for long sweeps.

## [0.1.0] — 2026-06-03

Initial public release. A self-hosted, MCP-native memory service: it stores agent
thoughts in Postgres + pgvector and exposes them to any MCP-capable client.

### Added
- **Capture & hybrid search (M1).** Immutable `thoughts` store; retrieval fuses
  vector kNN (BGE-M3) and trigram lexical search via reciprocal rank fusion with a
  recency boost. Four MCP tools: `capture`, `search_thoughts`, `recent_thoughts`,
  `list_scopes`.
- **Async embedding pipeline.** `pending_embeddings` queue drained by a worker;
  pluggable embedder backend (Ollama / TEI / any OpenAI-compatible `/v1`).
- **Cross-encoder reranking (M3).** Optional TEI reranker stage with an A/B
  benchmark harness (nDCG@10 / MRR); silently degrades if unavailable.
- **Tagging sidecar (M4 / M4.1).** JSONB tags (people, entities, topics,
  action_items, dates_mentioned, kind) written per thought; advisory, do not gate
  retrieval. Scope-aware controlled-vocabulary hinting. Content-fingerprint
  (SHA-256) dedup makes `capture` idempotent.
- **Relations graph (M5 / M5.2).** Thought-to-thought and polymorphic
  (entity / person / URL) links over a seven-relation closed vocabulary
  (`replaces`, `requires`, `references`, `supports`, `belongs_to`, `decided_by`,
  `refines`); soft-delete with audit. MCP tools `link_thoughts`,
  `unlink_thoughts`, `get_related_thoughts`, `get_thought`, `retract_thought`.
- **Stats CLI + tagger-extracted relations (M6 / M6.1).** `kengram stats` corpus
  and storage telemetry; the tagger auto-emits non-thought relations with
  `source='tagger'`.
- **Backup & restore (M7.0).** `kengram backup` / `kengram restore` with a
  manifest sidecar that validates schema head and warns on embedder/tagger drift.
- **Operator tooling.** Layered TOML + env config (`config/kengram.example.toml`),
  numbered sqlx migrations with a `migration_audit` table, and the
  `kengram serve` / `worker` / `migrate` / `embed-backfill` / `tag` / `bench` /
  `audit migrations` subcommands.

### Known limitations
- Single-user, single-session by design. No multi-tenant support and no web UI.
- No application-level auth yet (Tier 2 bearer tokens are planned). Default
  deployment is localhost-only (Tier 0).
- No Prometheus metrics or eval suite yet (remaining M7 scope).
- Tagger output quality varies by base model; tags are advisory only.

[Unreleased]: https://github.com/muckers/kEngram/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/muckers/kEngram/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/muckers/kEngram/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/muckers/kEngram/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/muckers/kEngram/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/muckers/kEngram/releases/tag/v0.1.0
