# M2 — Progress

Living checklist tracking M2 implementation. Each phase ends in a runnable, reviewable checkpoint. Items are checked off as they land; the **History** section at the bottom captures dated notes — decisions made in passing, surprises, things deferred. The companion design doc is `m2-facts-pipeline.md` in this directory; the operator's 12 inline-answered open questions there are the binding decisions this plan is built on.

## Operator decisions captured (from m2-facts-pipeline.md)

| # | Question | Decision |
|---|---|---|
| 1 | Async embedding mechanism | `pending_embeddings` queue table, `SELECT FOR UPDATE SKIP LOCKED` drain |
| 2 | Reflector batching | "Thoughts without facts" (LEFT JOIN IS NULL), ASC by `created_at` |
| 3 | Extractor prompt design | OpenAI `response_format` JSON Schema |
| 4 | `source_run_id` | **Yes**, with a `reflector_runs` table backing it |
| 5 | Dual-extractor disagreement | Defer entirely to M5 |
| 6 | Facts search strategy | Same RRF hybrid as `search_thoughts` |
| 7 | `Extractor` trait location | `engram-core` |
| 8 | Worker process structure | One `engram worker` process, two Tokio tasks |
| 9 | vLLM unreachable | Per-thought soft-fail, log, continue |
| 10 | `correct_fact` provenance | `extractor_model = "manual"`, version 0 |
| 11 | Cron scheduler crate | `tokio-cron-scheduler 0.15` |
| 12 | `search_facts` response shape | Include source thought content + scope + created_at |

**Other settled sub-decisions:**

- `engram embed-backfill` (M1's CLI) **survives** as a manual one-shot drain escape hatch — semantics unchanged.
- `capture`'s `embedding_status` becomes `"pending"` as the *normal* return (was the exception case in M1). MCP wire shape unchanged.
- `reqwest 0.13.3` upgrade landed in its own commit before M2 Phase A starts (`ddd3aad`).

## Phase A — Foundation

End state: migration applied; new crate compiles; types and traits exist; nothing wired up yet.

- [x] Migration `0002_facts_pipeline.sql`:
  - [x] `pending_embeddings` queue: `(id UUID PK, target_kind TEXT, target_id UUID, model_id TEXT, enqueued_at TIMESTAMPTZ, attempts INT, last_attempt_at TIMESTAMPTZ, last_error TEXT)`
  - [x] `facts_review_queue`: `(id UUID PK, statement, subject, predicate, object, confidence, source_thought_id, extractor_model, extractor_version, source_run_id, created_at, reviewed_at, decision TEXT)` — `decision` ∈ `pending|accept|reject`
  - [x] `reflector_runs` table: `(id UUID PK, started_at, finished_at, extractor_model, extractor_version, scope_filter TEXT, n_thoughts_processed INT, n_facts_committed INT, n_review_queue INT, error TEXT)`
  - [x] Add `source_run_id UUID REFERENCES reflector_runs(id)` to `facts` (nullable for `manual` rows)
  - [x] Index `pending_embeddings_dequeue_idx (enqueued_at ASC)` for FIFO drain
  - [x] Index `facts_review_queue_pending_idx (created_at ASC) WHERE decision = 'pending'`
- [x] New crate `engram-extract`:
  - [x] Workspace member added in root `Cargo.toml`
  - [x] Compiles empty
- [x] `engram-core` additions:
  - [x] `ExtractedFact { statement, subject?, predicate?, object?, confidence }` type
  - [x] `ExtractionContext { scope, max_facts }` type
  - [x] `Extractor` trait with `model_id()`, `version()`, `async extract(thought, ctx) -> Result<Vec<ExtractedFact>, ExtractorError>`
  - [x] `ExtractorError` enum with `is_transient()` classification (Timeout, Unreachable, 5xx are transient — mirror `EmbedderError`)
- [x] `cargo test --workspace`: 114 passing (was 106; +8 new `extractor` tests)
- [x] `cargo clippy --all-targets -- -D warnings`: clean

## Phase B — Async embedding seam

End state: capture handler no longer calls `Embedder::embed` inline; it enqueues. Worker process drains. New thought visible by trigram immediately; vector kNN within one worker tick.

- [x] `engram-storage` repository functions:
  - [x] `enqueue_embedding(pool, target_kind, target_id, model_id)` (returns bool — newly enqueued vs idempotent no-op)
  - [x] `claim_pending(pool, batch_size) -> Vec<PendingJob>` — single-statement atomic UPDATE … FROM (SELECT … FOR UPDATE SKIP LOCKED). No long-held tx required (the engineer's lean turned out to be cleaner than the "inside a tx" wording in the original checklist).
  - [x] `mark_embedded(pool, pending_id)` (DELETE)
  - [x] `mark_failed(pool, pending_id, error_msg)` (UPDATE last_error; attempts already bumped by claim)
  - [x] `enqueue_unembedded_thoughts(pool, model_id, scope?, limit)` (heal pre-M2 thoughts)
  - [x] `count_pending(pool)` (queue depth)
  - [x] Idempotency fix: `insert_thought_embedding` now `ON CONFLICT DO NOTHING` so crash-replay (worker dies between insert and mark_embedded) is harmless.
  - [x] `sqlx::test`: enqueue + claim + mark_embedded round trip; SKIP LOCKED smoke test via two-conn-with-tx pattern
- [x] `engram-mcp::capture` rewritten:
  - [x] No `Embedder` dependency in capture path — signature is `capture(pool, model_id, request)`
  - [x] Insert thought, enqueue, return `embedding_status: "pending"`
  - [x] `sqlx::test`: capture writes thought + pending_embeddings row; no embedding row yet
- [x] `engram-cli` `worker` subcommand:
  - [x] Embed-drainer task: loop, claim up to N, embed via `Embedder::embed`, persist, mark embedded; on transient failure log + mark_failed; on permanent failure log + leave (operator inspects)
  - [x] Configurable tick interval (default 5s) and batch size (default 16)
  - [x] Graceful shutdown on ctrl-c — `CancellationToken` + `tokio::task::JoinSet` + 30s deadline (so Phase C can `set.spawn(reflector_loop)` without refactor)
- [x] `engram-mcp::backfill` (M1's `embed-backfill`) rewritten as heal-then-drain over `pending_embeddings`
- [x] End-to-end test: capture-then-drain end-to-end via `EngramServer` tools (`capture_then_drain_makes_thought_indexed_via_get_thought`)
- [x] DEVELOPMENT.md: section on running `engram worker` alongside `engram serve`

## Phase C — Extractor + reflector

End state: vLLM-driven extractor produces facts; reflector cron walks unfacted thoughts and writes facts with run provenance; review queue receives low-confidence rows.

- [x] `engram-extract` impls:
  - [x] `OpenAICompatibleExtractor` — `/v1/chat/completions` with `response_format: { type: "json_schema", json_schema: {...} }`; default `endpoint = http://localhost:8000/v1`; default model name from config
  - [x] OpenRouter support — same Rust type, named-constructor preset (`OpenAICompatibleConfig::open_router(api_key, model)`) rather than a separate type. Avoids near-duplicate code. The CLI's `extractor.provider = "openrouter"` chooses the preset.
  - [x] Tests with `wiremock`: valid response → facts; malformed JSON → MalformedResponse; 5xx → transient Backend; 4xx → non-transient Backend; bearer auth header verified; system prompt substitution verified; misconfigured-endpoint check
  - [x] **`integration` feature**: live test against running vLLM (skipped by default, run with `--features integration`)
- [x] `engram-storage` repository functions for facts:
  - [x] `start_run(pool, extractor_model, extractor_version, scope_filter) -> RunId`
  - [x] `finish_run(pool, run_id, n_processed, n_committed, n_review, error?)`
  - [x] `find_unfacted_thoughts(pool, scope?, limit) -> Vec<Thought>` (LEFT JOIN facts IS NULL, ASC by created_at)
  - [x] `insert_fact(pool, NewFact)` with `extractor_model`, `extractor_version`, `source_run_id`, `confidence`
  - [x] `insert_review_queue_row(pool, NewReviewRow)`
- [x] Reflector task (in `engram-cli` worker):
  - [x] `tokio-cron-scheduler` (0.15.1) set up with default schedule from config (`0 0 3 * * *` — 6-field cron with seconds)
  - [x] On tick: `start_run`, walk unfacted thoughts in scope-order, call extractor per thought, soft-fail on extractor unreachable, route facts to `facts` or `facts_review_queue` per `review_queue_below` threshold, `finish_run`
  - [x] `sqlx::test` with `FakeExtractor` (analogue of `FakeEmbedder`): 8 tests covering high-confidence commit, low-confidence routing, source_run_id provenance, soft-fail path, idempotency on rerun, run counts, scope filter, explicit-facts override
- [x] Config:
  - [x] `[extractor]` section in `engram.toml`: provider, endpoint, model_name, model_id, model_version, api_key, timeout_seconds, temperature, max_facts_per_thought
  - [x] `[reflector]` section: enabled (default false), schedule, scope_filter, max_thoughts_per_run, max_facts_per_thought, review_queue_below
  - [x] Validation: `engram serve` doesn't require an extractor; `engram worker` only builds the extractor when `reflector.enabled = true`. Default-off keeps single-user dogfood drag-free.

## Phase D — MCP tools + manual reflect + dogfood

End state: M2 success criteria from m2-facts-pipeline.md met. Operator-driven dogfood ticked off after a week of use.

- [ ] `engram-mcp` tools:
  - [ ] `search_facts(query, scope?, limit?) -> { results: [{ fact_id, statement, subject?, predicate?, object?, confidence, source_thought_id, source_thought_content, source_thought_scope, source_thought_created_at, score }] }` — same RRF hybrid as thoughts, filters `WHERE superseded_at IS NULL`
  - [ ] `correct_fact(fact_id, replacement?) -> { new_fact_id?, superseded: bool }` — special `extractor_model = "manual"`, `extractor_version = 0`; sets `superseded_by`, `superseded_at` on old row; inserts new row pointing at same `source_thought_id` if `replacement` provided
  - [ ] `get_thought` updated to include `linked_facts: [...]` (rows where `source_thought_id = ?` and `superseded_at IS NULL`)
  - [ ] `sqlx::test`s for each: round trip, supersession audit, `search_facts` filters superseded
- [ ] `engram reflect` subcommand:
  - [ ] `engram reflect [--scope <s>] [--limit <n>]` — one-shot reflector run, exits when done
  - [ ] `engram reflect --rerun --scope <s> [--since <date>]` — re-extract historical thoughts; for each, if `(subject, predicate, object)` matches an existing non-superseded fact, **merge** (no new row); if it conflicts, supersede via `superseded_by`. Audit trail preserved.
  - [ ] `sqlx::test`: rerun twice produces identical fact set (idempotency criterion)
- [ ] Documentation:
  - [ ] README.md status table: M2 ✅ with brief sentence
  - [ ] DEVELOPMENT.md: vLLM prerequisites, `engram worker` runbook, `engram reflect` examples
  - [ ] Design doc revision-history entry
- [ ] **Operator-driven**: MCP smoke test from Claude Code / `mcp-inspector` invoking `search_facts`, `correct_fact`, and the updated `get_thought` against `engram serve` (with `engram worker` running in parallel)
- [ ] **Operator-driven**: real dogfood — run engram with extractor for ≥1 week, confirm fact rate and false-positive/-negative balance is acceptable, do at least one `correct_fact` round trip

## History

Dated notes appended as items land. Format: `YYYY-MM-DD — <one-line summary>`. Multi-line entries fine for decisions that need explanation.

<!-- Most recent entry first. -->

- **2026-05-13** — M2 Phase C landed. Reflector cron + concrete extractor impls are live. `OpenAICompatibleExtractor` (one Rust type, two named-constructor presets — `vllm_local()` and `open_router(api_key, model)` — rather than two separate types, mirroring how `OpenAICompatibleEmbedder` already covers Ollama/TEI/OpenAI by config). `FakeExtractor` with `Deterministic`/`Timeout`/`Unreachable`/`Misconfigured` behaviors plus `with_confidence(f32)` and `with_facts(Vec<ExtractedFact>)` constructors for routing/explicit tests. Five new storage functions (`start_run`, `finish_run`, `find_unfacted_thoughts`, `insert_fact`, `insert_review_queue_row`) + `RunId`, `NewFact`, `NewReviewRow` types. `engram-mcp::reflect::run_reflector_once` orchestrates a single pass (start_run → LEFT-JOIN unfacted → for each thought: extract → route by `review_queue_below` → insert → finish_run). The cron loop wraps that call inside `tokio-cron-scheduler` 0.15.1 (latest stable per `cargo info`); `engram worker` runs the loop as a second `JoinSet` task alongside the embed-drainer when `reflector.enabled = true`. Three engineering decisions worth documenting: (1) one extractor type with two presets, not two types; (2) single-band routing (`review_queue_below` only) — m2-facts-pipeline.md's three-band design with a "flagged but committed" middle band would require a new column on `facts` that doesn't exist, so it's deferred; (3) `reflector.enabled` defaults to `false` so `engram worker` works without vLLM. Live smoke verified: with vLLM down, `engram worker` with `ENGRAM_REFLECTOR__ENABLED=true ENGRAM_REFLECTOR__SCHEDULE="*/3 * * * * *"` fires on cron, soft-fails per thought (Q9 path), and exits cleanly on SIGINT. Test count 129 → 166 (+37: storage +8, extract +17, mcp/reflect +8, cli config +4).

- **2026-05-12** — M2 Phase B landed. Async embedding seam in place: `capture` no longer takes an `Embedder` arg — it inserts the thought, enqueues a `pending_embeddings` row keyed by the active model id, and always returns `embedding_status: "pending"`. New `engram worker` subcommand drains the queue every 5s in a `tokio::task::JoinSet` (designed for Phase C's reflector task to plug in alongside it). `embed-backfill` rewritten as heal-then-drain (enqueues any unembedded thoughts → drains the queue, bounded by `--limit`). Three engineering refinements during synthesis: (1) `claim_pending` is single-statement `UPDATE ... FROM (... FOR UPDATE SKIP LOCKED)` rather than the originally-prescribed long-held tx — same SKIP LOCKED safety, no held connection; (2) `insert_thought_embedding` now `ON CONFLICT DO NOTHING` so a worker that crashes between embed and `mark_embedded` is harmless on replay; (3) `ExtractionContext` only carries scope + max_facts since the `Thought` is passed separately. Test count 114 → 129 (storage 20→29, mcp 29→35, plus a `WorkerConfig` default test). Manual smoke: `engram worker` starts cleanly, drains the queue every 5s, exits within ~1s of SIGINT.

- **2026-05-12** — M2 Phase A landed. Migration `0002_facts_pipeline.sql` applied cleanly (three new tables — `pending_embeddings`, `reflector_runs`, `facts_review_queue` — plus `facts.source_run_id` FK both ways). New `engram-extract` crate compiles empty; the `Extractor` trait + `ExtractedFact` + `ExtractionContext` + `ExtractorError` live in `engram-core`, mirroring `Embedder`/`EmbedderError` in shape and `is_transient()` discipline. One drift from the plan: dropped `source_thought_id` from `ExtractionContext` because the `Thought` is already passed as the first argument to `extract()` — carrying the id separately would be redundant. Workspace test count 106 → 114 (the 8 new `extractor` tests).

- **2026-05-12** — M2 design conversation closed. All 12 open questions in m2-facts-pipeline.md answered inline by RJF; only #4 diverged from the engineer's lean (operator opted **For** adding `source_run_id`, and during synthesis we agreed to back it with a small `reflector_runs` table so the data is actually queryable). Three additional sub-decisions settled: `engram embed-backfill` survives as an escape hatch; capture's `embedding_status` becomes `"pending"` as the normal return (semantic shift only — MCP wire shape unchanged); `reqwest 0.13.3` upgrade landed as its own commit (`ddd3aad`) before Phase A. Plan above is the next-conversation artifact; Phase A is the first concrete unit of work.
