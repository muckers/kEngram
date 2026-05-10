# M2 — Facts pipeline

## Goal

Engram derives structured facts from captured thoughts on a scheduled basis. The operator can search facts, correct wrong ones, and trust that the thoughts/facts split preserves provenance and supports re-extraction.

This is the milestone that takes engram beyond "search engine for thoughts" and toward "memory with structure." It also exercises the async-embedding seam designed (but not used) in M1.

## In scope

- The `Extractor` trait (in `engram-core`) plus two implementations: `OpenAICompatibleExtractor` (vLLM `/v1/chat/completions` with structured-output) and `OpenRouterExtractor` (cloud fallback).
- Worker process: a new `engram worker` subcommand. Long-running; runs the reflector cron + drains async-embedding queue.
- Async embedding: capture posts a job (in `pending_embeddings` or via NOTIFY/LISTEN; mechanism TBD); worker drains and calls `Embedder::embed`. Capture returns immediately with the thought ID. Brief window where the thought is searchable by trigram only.
- Reflector: scheduled task that walks recent thoughts in a scope, calls `Extractor::extract`, writes facts with `extractor_model`, `extractor_version`, `confidence`.
- Confidence-gated commit: facts below `review_queue_below` go to a review queue; between that and `min_confidence_to_store` are flagged but committed; above are committed normally.
- Two new MCP tools: `search_facts`, `correct_fact`. `get_thought` now joins linked facts.
- New CLI subcommands: `engram worker`, `engram reflect [--rerun] [--scope <s>] [--since <date>]`.
- Optional dual-extractor reconciliation (`extractor.dual_run = true`): commit only facts that two distinct extractors both produce.

## Out of scope (deferred to which milestone)

- Cross-encoder reranker → **M3**
- Artifact ingestion → **M4**
- `engram audit` reports, human review UI, eval suite, Prometheus metrics, Tier 2 auth → **M5**
- Knowledge-graph reasoning → out of scope indefinitely

## Schema impact

Migration `0002_facts_pipeline.sql` adds:

- A `pending_embeddings` queue table (or equivalent NOTIFY/LISTEN setup; design TBD in M2 planning).
- A `facts_review_queue` table for low-confidence facts.

The existing `facts` table is now populated by code. No structural change to `facts` itself.

## MCP surface delta

- `search_facts(query: string, scope?: string, limit?: int) -> { results: [{ fact_id, statement, subject?, predicate?, object?, confidence, source_thought_id, score }] }`
- `correct_fact(fact_id: uuid, replacement?: { statement, subject?, predicate?, object? }) -> { new_fact_id?: uuid, superseded: bool }` — if `replacement` is provided, writes a new fact pointing at the same source; marks the old one superseded. If omitted, just supersedes (effectively delete-by-supersede).
- `get_thought(thought_id)` response now includes `linked_facts: [...]` populated from `facts WHERE source_thought_id = ?`.

## Crate structure delta

- **New crate: `engram-extract`.** Defines the `Extractor` trait (moved from `engram-core` — or kept in `engram-core` and re-exported, TBD) and concrete impls `OpenAICompatibleExtractor`, `OpenRouterExtractor`. JSON-Schema response handling lives here.
- **`engram-cli`** gains the `worker` and `reflect` subcommands; the `serve` subcommand learns to refuse async-embedding work (it goes to the worker).
- **`engram-storage`** gains repository functions for facts: insert with provenance, search facts (vector + trigram fused, similar shape to thoughts), supersede a fact, query the review queue.
- **`engram-mcp`** gains the two new tool handlers.

## Dependencies

- **Prior milestones:** M1 (capture, search, embedder, MCP scaffold).
- **External services:** vLLM serving an instruct model on `:8000/v1` (or an OpenRouter API key for cloud fallback). vLLM was not required in M1; it's required from M2.

## Success criteria

1. Reflector runs on schedule; produces facts with confidence; respects review-queue thresholds.
2. `search_facts` returns relevant facts for a query that the underlying thoughts cover.
3. `correct_fact` correctly supersedes a prior fact (`superseded_by`, `superseded_at` set on the old row; new row inserted) and `search_facts` no longer surfaces the old one (assuming it filters `WHERE superseded_at IS NULL`).
4. **Async embedding correctness:** capture a thought while TEI is down. Capture succeeds and returns a thought ID. Bring TEI back up. Within one worker tick, the embedding row appears and `search_thoughts` finds the thought via vector.
5. **Re-extraction idempotency:** `engram reflect --rerun --scope work --since 2026-01-01` run twice produces the same `facts` table (same rows; same supersession history; no duplicate facts).
6. **Operator dogfood:** the operator runs M2 for at least a week, has at least one `correct_fact` round-trip, and is satisfied with the rate of false-positive vs. false-negative facts.

## Open questions

- **Async embedding mechanism.** A dedicated `pending_embeddings` queue table polled by the worker, vs. PostgreSQL `LISTEN/NOTIFY` from the capture side, vs. an external queue (`apalis`, `pgmq`)? Each has different failure-mode and back-pressure characteristics.
- **Reflector batching strategy.** Per-scope round-robin, strict by `created_at`, or per-thought as soon as it's "old enough"? What does "recent thoughts" mean — within the last cron interval, or "all thoughts that don't yet have any facts"?
- **Extractor prompt design.** JSON Schema response format vs. grammar-constrained vs. free-text + parse. Vendor-coupling considerations.
- **Facts table — should it grow a `source_run_id`?** So all facts produced in one reflection run can be jointly retracted if the run is later judged bad. Possibly, possibly later.
- **Dual-extractor disagreement handling.** Store both with a flag, store neither, or store the higher-confidence one and queue the disagreement for review?
- **Search strategy for facts.** Same RRF hybrid as thoughts? Or weighted toward exact-statement match because facts are shorter and more structured?
- **Trait location for `Extractor`.** In `engram-core` (alongside `Embedder`) or in `engram-extract` (where it lives)? Hinges on whether `engram-core` should ever depend on `engram-extract`.
