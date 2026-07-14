# M9 — Notes index (artifacts, resurrected)

**Status:** REVIEWED 2026-07-14 — design pressure-tested against the codebase (schema claims verified; retrieval-coupling cost and re-ingest lifecycle gaps folded in below); operator decisions recorded in Open questions. Ready for implementation planning; not yet scheduled.

**One-line:** ingest a Logseq 1.x markdown graph as artifacts + block-level chunks, embedded into the same vector space as thoughts, searchable through an opt-in `include_artifacts` flag on `search_thoughts` — the files stay canonical on disk; kEngram is the derived, disposable index over them.

## Design pivot (why artifacts is back)

The original artifacts milestone (`m6-artifacts.md`, dropped 2026-05-17) proposed generic long-form ingestion — PDFs, transcripts, web pages — fused unconditionally into `search_thoughts`. It was dropped on three grounds: M5.2's `to_url` link target covered "this thought references that doc," a live-corpus measurement showed kengram occupies a high-signal-density sweet spot that indiscriminate ingestion would dilute, and stats + tagger relations were more pressing.

M9 resurrects the capability with a different design center and an answer to each objection:

1. **A concrete driving use case.** The operator is a Logseq user. Logseq 2.0 abandons plain-markdown-files-as-canonical for a database with markdown as an export format. The property being lost is not "no database" — Logseq 1.x always had one (DataScript, in memory) — it is *files are canonical and the database is derived and disposable*. M9 restores exactly that property with kEngram as the index: hybrid semantic search, the M8 link-graph/web read surface, and agent access to the notes over MCP, none of which Logseq ever offered.
2. **The dilution objection is answered in retrieval design, not by refusing to ingest.** Chunks share the store but not the default ranking: `search_thoughts` results are byte-identical to pre-M9 behavior unless the caller passes `include_artifacts: true`. The thought corpus's signal density is untouched by default.
3. **The schema runway already exists.** Migration 0001 shipped `artifacts` and `artifact_chunks` (inert ever since — the "future drop migration" was never written), and `embeddings.target_kind` already admits `'artifact_chunk'`. The HNSW partial index predicate is on `model_id`, not target kind, so chunk embeddings are covered with no index change.

## Design principles

**Two-class content model.** This is the spine of the milestone; every scope decision below derives from it.

| | Thoughts | Notes (artifacts) |
|---|---|---|
| Author | agents, via MCP `capture` | human, in the Logseq editor |
| Canonical store | kEngram (immutable, DESIGN.md §10) | markdown files on disk |
| Mutation | never; `refines` / `replaces` | edited freely; kEngram **re-derives** on re-ingest |
| Tags | full tagger sidecar | none — embeddings + trigram only |
| Unit of retrieval | thought | chunk (≈ a Logseq block subtree) |
| Default search | in | out; opt-in via `include_artifacts` |

**Disposability invariant.** Drop every `artifacts` / `artifact_chunks` / chunk-embedding row and re-ingest the folder ⇒ identical state. The index must be rebuildable from the files alone, always. (This is the existing "idempotent, re-runnable" project rule applied to a new pipeline.)

**kEngram is never the editor.** No write or edit surface for note content, in any milestone. The web surface stays read-only; `psql` stays the admin interface; the files are edited wherever the operator likes. Ingest (creating the *derived* index) is the only write, via CLI/MCP.

**No thoughts are minted from notes.** Chunks are a parallel embedded corpus. Capturing thoughts from note content would strand stale immutable thoughts on every file edit and flood the thought corpus — the original dilution problem through the front door. Chunks get replace-on-change semantics instead, which immutable thoughts cannot and should not have.

## Goal

The operator points `kengram ingest` at a Logseq 1.x graph directory. Every markdown page becomes an artifact; every block subtree becomes a chunk with provenance (file path, block `id::`, position); every chunk is embedded with the active embedder. `search_thoughts(query, include_artifacts: true)` returns thoughts and note-blocks fused under one RRF + rerank ranking, each chunk hit resolvable back to the exact block in the source file. Editing a note and re-ingesting replaces that file's chunks; nothing else moves.

## In scope

- **Logseq markdown parser** (`kengram-core`): outline bullets into a block tree; `id::` block properties; `[[wikilinks]]`; `((block refs))`; page properties (`property:: value` head block); journal pages (date from filename). Parsed links/refs land in chunk metadata (see out-of-scope for edge materialization). Parser hygiene: decode Logseq namespace filenames (`%2F` / `___` → `/`) for titles; skip `logseq/`, `bak/`, `.recycle/`, and `assets/`; strip block-property lines (`id::`, `collapsed::`, …) from chunk content — they are embedding noise — while preserving them in metadata.
- **`Chunker` trait** (`kengram-core`, pure) + v1 **outline-block-aware impl**: chunk on block-subtree boundaries, merging small sibling blocks up to a token budget (~500 target, configurable), never splitting a block mid-way except for oversized single blocks. `[chunker]` TOML section. **Embed-text composition:** `artifact_chunks.content` stores the raw block text only (clean for display and trigram); the page-title + ancestor-path context header lives in provenance metadata, and both the embed drainer and the rerank passage accessor compose `header + content` deterministically from metadata — embed and rerank see the same text, and the composition is rebuildable (disposability holds).
- **Chunk provenance metadata** (JSONB per chunk): source file path relative to graph root, block `id::` UUID when present, ancestor path, line span, journal date for journal pages, outbound `[[wikilinks]]` / `((refs))`.
- **CLI ingest**: `kengram ingest <path> [--scope S] [--dry-run]`. Directory mode is primary (recurse `pages/` + `journals/`, `.md` only); single-file mode works. Chunking + row writes are synchronous in the CLI; embeddings enqueue through the existing `pending_embeddings` queue and drain via the existing worker. The queue is already polymorphic (`target_kind` + bare `target_id`, migration 0002); only the drainer is thought-bound — extend `process_embed_job`'s match (`kengram-mcp/src/drain.rs`) with an `artifact_chunk` arm plus a new `fetch_artifact_chunk` repo fn. **Queue priority:** a first ingest enqueues tens of thousands of chunk jobs and `claim_pending` is strict FIFO, which would starve thought captures of their embeddings for the whole drain; prioritize `target_kind='thought'` in `claim_pending`'s ordering (or interleave kinds per batch) so thoughts never wait behind a bulk backlog. **Parse failure on a changed file:** keep the previous good chunks and mark the artifact `failed` — the index stays consistent with the last good state rather than dropping coverage.
- **Idempotent re-ingest**: per-file SHA-256 fingerprint. A file is skipped only when **both** its content fingerprint **and** its stored `chunker_version` (which folds in the effective `[chunker]` config, or is accompanied by a config hash) match — fingerprint alone would let a chunker config change silently never apply to unchanged files, breaking the disposability invariant. Changed file → replace its chunks and their embeddings (delete + reinsert under the same artifact). File deleted from disk → artifact and chunks removed on the next **directory-mode** ingest of that scope (removal set = artifacts in the given `--scope` whose `source_path` was not seen this run; single-file ingest never removes anything). **Orphan cleanup is explicit:** `embeddings.target_id` and `pending_embeddings.target_id` are soft references with no FK cascade from `artifact_chunks`, so chunk replacement/removal must delete the corresponding `embeddings` rows (orphaned vectors bloat the HNSW index and silently eat candidate slots) and the drainer must treat a missing chunk as job-complete/drop, never a retry loop. Deterministic, documented, tested.
- **Retrieval**: `search_thoughts` gains `include_artifacts: bool` (default `false`). When true: the **existing two legs each go mixed** — the vector leg UNION ALLs a chunk arm alongside the thought arm under a single ORDER BY + LIMIT, likewise the trigram leg over `artifact_chunks.content` (new GIN `gin_trgm_ops` index). Two mixed legs, not four per-kind legs: four fused lists would change RRF score semantics versus today's two, and per-kind legs would hand chunks an independent rank ladder (dilution through the back door). RRF fusion and cross-encoder rerank then operate identically on both (query/passage pairs; the chunk passage is the composed header + content). **Chunks get no recency boost** (operator decision 2026-07-14): re-ingest rewrites chunk rows, so `created_at`-based boosting would hand every note maximum freshness after each cron run and swamp thoughts; chunks rank by RRF + rerank alone, and the resulting slight pre-rerank tilt toward recent thoughts is consistent with the dilution guard (rerank is the final discriminator). Result items gain `target_kind: "thought" | "artifact_chunk"`; chunk hits carry `artifact_id`, `chunk_index`, chunk content, and provenance metadata instead of thought fields.
- **Scoping**: artifacts take a `scope` like thoughts (e.g. `rjf.notes`); existing `scope` / `scope_prefix` filters compose with `include_artifacts`. Artifact identity is `UNIQUE (scope, source_path)` — which implies **one graph per scope**: ingesting a second graph into the same scope clobbers the first on the deletion-mirror pass. `artifact_chunks` has no `scope` column; the chunk legs filter scope via the join to `artifacts` (no denormalized column).

## Out of scope (deferred to which milestone)

- PDF / HTML / `https://` fetchers, generic non-Logseq ingestion → **M9.x fast-follow** (the pipeline is content-agnostic below the parser; only parsers/fetchers are missing).
- Web UI artifact detail view (render the note, highlight the matched block) + chunk hits in the web search page → **M9.x fast-follow** (read-only, fits M8's frame).
- Materializing `[[wikilinks]]` / `((block refs))` as real graph edges → **M10 candidate**. Requires an `artifact_chunk` link-target kind on `thought_links` (schema + vocabulary work). v1 keeps them queryable in chunk metadata JSONB.
- Per-chunk or per-artifact tagging → deferred until dogfood shows `tag_filter` is missed on notes. Tags remain a thought-level concept; a 50-page ingest must not enqueue 60 LLM tag jobs.
- `ingest_artifact` MCP tool → **decided: CLI-only v1** (mirrors the `kengram stats` CLI-first precedent; see resolved open question 1). MCP ingest deferred until dogfood shows the need.
- File-watcher / daemon auto-ingest → indefinitely; re-ingest is a manual or cron-driven CLI run.
- Audio/video transcription, OCR, web crawling → indefinitely (unchanged from the original doc).
- Agent transcripts (DESIGN.md open question 9) → not in v1; the notes pipeline may inform it later.
- Any note write/edit surface → never (design principle above).

## Schema impact

No new tables. Migration `0012` amends the inert 0001 tables:

- `artifacts`: add `content_fingerprint BYTEA` (SHA-256 of file bytes), `source_path TEXT` (path relative to graph root; identity is `UNIQUE (scope, source_path)` — `content_uri` keeps the absolute `file://` form but is informational only, since it is machine-specific), `status TEXT` (`ingested` | `failed`), `updated_at TIMESTAMPTZ`.
- `artifact_chunks`: add `metadata JSONB NOT NULL DEFAULT '{}'` (provenance), `chunker_version INT NOT NULL` (idempotent re-chunking discipline, mirrors tagger provenance), `created_at TIMESTAMPTZ`.
- New index: `artifact_chunks_content_trgm_idx` GIN `gin_trgm_ops` on `artifact_chunks.content` (trigram leg parity).
- `embeddings`: **unchanged** — `target_kind='artifact_chunk'` rows just start existing; the bge-m3 HNSW partial index already covers them (predicate is `WHERE model_id = 'bge-m3:1024'`, verified `0001_initial.sql:60-62`). Note there is no FK from `embeddings`/`pending_embeddings` to `artifact_chunks` — cleanup on chunk replacement is the ingest code's job (see Idempotent re-ingest).

Exact column set to be finalized in the implementation plan against `0001_initial.sql:25-42` (verified 2026-07-14: both tables are exactly as shipped in 0001, untouched by 0002–0011, zero Rust surface beyond the `target::ARTIFACT_CHUNK` constant).

## MCP surface delta

- `search_thoughts` gains `include_artifacts?: bool` (default `false`; absent = today's behavior, bit-for-bit). Implementation note for the bit-for-bit promise: `search_response_json` emits every key via `json!`, so the new `target_kind` / chunk fields must be **conditionally inserted** (omitted on the thought-only path, not emitted as null); the regression test is a golden-snapshot comparison of serialized responses.
- Result shape gains `target_kind`; chunk hits carry `{ artifact_id, chunk_index, content, metadata (provenance), scope, artifact_title }` and no thought-only fields.
- **No new MCP tools in v1** (pending the open question on `ingest_artifact`).

## CLI surface delta

- New subcommand: `kengram ingest <path> [--scope S] [--dry-run]`. Prints a summary in the `kengram stats` plain-println style: files seen / unchanged / re-ingested / removed, chunks written, embeddings enqueued.
- `kengram stats` gains artifact/chunk counts and the chunk-embedding queue depth (it is the operator's telemetry surface). Known v1 quirk: `list_scopes` counts thoughts only, so a notes-only scope is invisible to agents — acceptable for v1, artifact-aware scope counts are an M9.x candidate.

## Crate structure delta

- **`kengram-core`**: Logseq markdown parser (block tree), `Chunker` trait + outline-block impl, `Artifact` / `ArtifactChunk` domain types. Pure; no I/O. **Plus the retrieval-type rework — the biggest single implementation cost of the milestone:** `search::Hit` wraps a concrete `Thought` and `rrf_fuse` keys its accumulator by `ThoughtId`; both need to move to an enum payload (`Thought | ChunkHit`) keyed by `(target_kind, uuid)`, with a kind-agnostic passage accessor for rerank.
- **`kengram-storage`**: artifact/chunk repository fns (incl. `fetch_artifact_chunk` for the drainer), fingerprint lookup, replace-chunks-for-artifact (with explicit embedding cleanup), migration 0012. Both leg queries are thought-bound today (vector leg hardcodes `target_kind='thought'` in its JOIN; trigram leg is `FROM thoughts`) and gain UNION ALL chunk arms behind `include_artifacts`. `pending_embeddings` needs no schema change (already polymorphic); `claim_pending` gains thought-priority ordering.
- **`kengram-embed`**: unchanged in shape; the `Embedder` sees chunk text as ordinary content.
- **`kengram-mcp`**: `include_artifacts` through the search orchestrator (which also feeds `/api/*`, so the web surface inherits it for free when M9.x lands).
- **`kengram-cli`**: `ingest` subcommand; worker embed-drainer handles chunk targets.
- **`kengram-web`**: untouched in v1.
- **No new crate.**

## Dependencies

- **Prior milestones:** M1 (storage, embedder, hybrid search), M3 (rerank), M8 only for the fast-follow web view.
- **External services:** none new; TEI continues to provide embeddings. The tagger and vLLM are not involved.
- **New crates (deps):** token-budget counting uses a cheap heuristic (≈ chars/4) rather than the `tokenizers` crate — the budget is soft, and the heuristic avoids a heavy dependency plus a bundled tokenizer model file. Revisit only if chunk-quality dogfood demands real token counts. No Python/Node, per project rules.

## Success criteria

1. **Real-graph round-trip:** ingest the operator's actual Logseq graph folder; queries the operator derives from their own notes return the right blocks in the top 10 with `include_artifacts: true`.
2. **Dilution guard, provable:** with `include_artifacts` absent or false, `search_thoughts` responses are identical to pre-M9 output on the same corpus (regression-tested via golden-snapshot comparison of serialized responses, not just asserted).
3. **Edit round-trip:** edit one note file, re-ingest the directory → only that file's chunks are replaced; no duplicates; unchanged files are no-ops; a deleted file's chunks disappear — **and no orphaned `embeddings` or stuck `pending_embeddings` rows survive the replacement** (verified by row counts, not just search output).
4. **Disposability:** truncate artifacts/chunks/chunk-embeddings, re-ingest → state identical to the first ingest (fingerprints, chunk counts, search behavior). Also holds across a `[chunker]` config change: bumping the config re-chunks on the next ingest, so incremental state never diverges from a fresh rebuild.
5. **Provenance:** any chunk hit resolves to file path + block `id::`/line span sufficient to open the exact block in an editor.
6. **Hygiene:** `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, full workspace tests green; integration tests cover ingest + fused search against real Postgres + TEI.

## Open questions (operator answers recorded 2026-07-14)

1. **`ingest_artifact` MCP tool: v1 or fast-follow?** ✅ **RESOLVED: CLI-only v1.** Matches the stats precedent and keeps agents from writing artifacts; MCP ingest deferred until dogfood shows the need.
2. **Scope naming for the graph.** ✅ **RESOLVED: single operator-chosen scope per ingest run** (`--scope`, required), e.g. `rjf.notes`. One graph per scope (see Scoping).
3. **Chunk token budget + merge policy.** OPEN (dogfood-tunable): ~500-token target with sibling-merge is the strawman; journals (many tiny blocks) vs long-form pages may want different budgets. Tunable via `[chunker]`; defaults need dogfood.
4. **Tag escape hatch trigger.** Default stands unless the operator objects: if dogfood shows notes need `tag_filter`, the escape hatch is one artifact-level tag job (head-sample), not per-chunk. Per-chunk tagging stays off the table.
5. **Sync vs queued chunking.** ✅ **RESOLVED: sync CLI chunking, async embeddings via the existing queue.** Operator's graph is 1k–10k pages with cron-driven re-ingest — sync chunking is fine at that scale, the fingerprint no-op fast path keeps cron runs cheap, and the thought-priority queue ordering (see CLI ingest) covers the bulk-backlog case.
6. **Chunk recency (found in review).** ✅ **RESOLVED: chunks get no recency boost** — see Retrieval. Journal-date-based recency is a possible later refinement.
