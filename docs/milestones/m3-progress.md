# M3 — Progress

Living checklist tracking M3 implementation. Each phase ends in a runnable, reviewable checkpoint. Items are checked off as they land; the **History** section at the bottom captures dated notes — decisions made in passing, surprises, things deferred. The companion design doc is `m3-search-quality.md` in this directory; the operator's settled questions there are the binding decisions this plan is built on.

## Operator decisions captured (from m3-search-quality.md + 2026-05-14 pre-M3 design pass)

| Decision | Resolution |
|---|---|
| Phasing model | M2-style A/B/C/D, each its own focused planning conversation |
| Phase A scope | pipeline-quality wins (v3 prompt, `commit_or_supersede`, `extract` flag, `n_extractor_failures`, `(S, P, O)` trigram) |
| Phase B scope | retrieval quality (cross-encoder reranker, fact embeddings, A/B harness) |
| Phase C scope | deeper pipeline quality (subsumption, structured relations, per-claim retraction durability, three-band routing) |
| Phase D scope | operator dogfood + close-out |
| Per-claim retraction durability — architecture | inherit retraction state at insert time (composes with the 2026-05-14 dedup-via-supersession work); negative-claim registry rejected |
| `extract` metadata flag — `"durable-only"` semantics | inject extra system message at reflect time (reinforces the bundled prompt's mixed-content rule per-thought) |
| Phase A commit style | single bundled commit `M3 Phase A: pipeline-quality fixes` |

## Phase A — Pipeline-quality fixes

End state: dogfood-driven SPO bugs corrected at the prompt level; within-call dedup eliminates the `run_reflector_once` duplicate class; operators can mark thoughts at capture time to skip or filter extraction; reflector_runs surfaces extractor-failure observability; trigram search reaches into the (S, P, O) triple.

- [x] Migration `0004_reflector_runs_failures.sql`: `reflector_runs.n_extractor_failures INT NOT NULL DEFAULT 0`.
- [x] Storage: `finish_run` signature extended with `n_failures: i32`; SQL UPDATE persists the new column. Test: `finish_run_persists_n_extractor_failures`.
- [x] Storage: `search_facts_trigram` lexical scoring now consults `statement || subject || predicate || object` via `word_similarity` (window-based, not whole-string set-similarity). Test: `search_facts_trigram_matches_via_triple_when_statement_does_not_mention_query`.
- [x] `engram-core`: `ExtractMode { All, DurableOnly }` enum; `ExtractionContext` gains an `extract_mode` field and `with_extract_mode(...)` builder method (back-compat: `::new` defaults to `All`).
- [x] `engram-extract`: `OpenAICompatibleExtractor::extract` appends a `DURABLE_ONLY_HINT` system message when `ctx.extract_mode == DurableOnly`. `ChatRequestBody.messages` changed from fixed `[ChatMessage; 2]` to `Vec<ChatMessage>` to accommodate the optional injection.
- [x] `engram-extract`: `FakeExtractor` records the last received `ExtractionContext` for test introspection.
- [x] `engram-extract`: `BUNDLED_SYSTEM_PROMPT` rewritten to v3 — SPO decomposition rules (comparative S/O mapping, self-referential rejection, conditional-as-subject), tighter confidence rubric (declarative 0.9-1.0, hedged 0.7-0.9, conditional 0.5-0.7), two new episodic-skip negatives (single-benchmark measurements, hardware-spec metadata), JSON envelope shape restated in prose. `model_version` 2 → 3 in `vllm_local()` and `open_router(...)` presets.
- [x] `engram-cli`: `ExtractorConfig::default::model_version` 2 → 3.
- [x] `engram-mcp/reflect`: factored `commit_or_supersede` helper carrying the four-case decision tree from `run_reflector_rerun`; applied to `run_reflector_once`. Both functions now route through the same dedup-via-supersession logic. Test: `once_supersedes_when_statement_matches_but_triple_differs_within_call` (ports the rerun-side regression to first-time extraction).
- [x] `engram-mcp/reflect`: `extract_directive` helper reads `metadata.extract` and branches: `"none"` → `ExtractDirective::Skip` (no extractor call); `"durable-only"` → `Run(ExtractMode::DurableOnly)`; absent/`"all"`/unknown → `Run(ExtractMode::All)`. Both `run_reflector_once` and `run_reflector_rerun` consult this before each thought. Tests: `reflector_skips_thought_with_extract_none`, `reflector_propagates_durable_only_via_context`, `reflector_treats_absent_extract_as_all`.
- [x] `engram-mcp/reflect`: both `run_reflector_once` and `run_reflector_rerun` pass `n_failures` to `finish_run` so the new column is populated on every run.
- [x] `DEVELOPMENT.md`: config example shows `model_version = 3` with the 2026-05-14 dated comment.
- [x] `docs/milestones/m3-search-quality.md`: Schema-impact section reflects the migration-numbering reality (`0004` shipped; `0005_facts_flagged.sql` slated for Phase C).
- [x] `cargo test --workspace`: 235 passing (was 229; +7 new − 1 deleted `finish_run_sets_finished_at` superseded by extended assert).
- [x] `cargo clippy --all-targets -- -D warnings`: clean.

**Operator-driven (post-merge):**

- [ ] Re-extract the 2026-05-14 dogfood corpus via `engram reflect --rerun` and verify the listed fact_ids:
  - SPO comparatives (`8da1fa45`, `64e26652`, `fb38bf42`, `51744197`, `e0238c2f`) have correct S=A / O=B mappings.
  - Self-referential triples (`39016e00`, `eeced4b3`, `582b76e1`) are no longer emitted; or, if emitted, have `subject != object`.
  - Conditional-as-subject (`103f44c9`, `bea3629d`, `e9032602`) have the named system as subject (not the conditional clause).
  - Episodic negatives (`e69eff9b`, `ec465660`) no longer extracted.
  - Confidence varies across hedged vs declarative claims rather than uniformly anchoring at 0.85.

## Phase B — Retrieval quality

Not yet planned. Items (per `m3-search-quality.md`):

- [ ] Cross-encoder reranker + TEI rerank-task deployment (L)
- [ ] Rerank stage in `search_thoughts` / `search_facts`
- [ ] Per-call rerank parameters (`rerank?: bool`, `candidate_pool?: int`)
- [ ] Fact embeddings (extend async-embedding seam; `target_kind = 'fact'`)
- [ ] Eval-suite-style A/B comparison harness

## Phase C — Deeper pipeline quality

Not yet planned. Items:

- [ ] Subsumption-aware dedup
- [ ] Structured relations in output schema
- [ ] Per-claim retraction durability (inherit-at-insert; composes with the 2026-05-14 dedup-via-supersession work)
- [ ] Three-band confidence routing with `facts.flagged` (migration `0005_facts_flagged.sql`)

## Phase D — Operator dogfood + close-out

Not yet planned. Items:

- [ ] Run M3 for ~1 week of real use.
- [ ] Evaluate against milestone-level Success criteria in `m3-search-quality.md`.
- [ ] Decide rerank-on-by-default vs off based on daily-use feel.
- [ ] Write the closing `m3-progress.md` History entry.
- [ ] Mark M3 ✅ in `README.md`.

## History

Dated notes appended as items land. Format: `YYYY-MM-DD — <one-line summary>`. Multi-line entries fine for decisions that need explanation.

<!-- Most recent entry first. -->

- **2026-05-14** — **M3 Phase A landed.** Single bundled commit `M3 Phase A: pipeline-quality fixes` covering v3 prompt revision (SPO decomposition rules: comparative S=A/O=B, self-referential subject-MUST-NOT-equal-object, conditional-as-subject; tighter per-hedging-level confidence rubric; two new episodic-skip negatives; JSON envelope restated in prose; `model_version` 2 → 3), `commit_or_supersede` helper factored out of `run_reflector_rerun` and applied to `run_reflector_once` (within-call dedup-via-supersession parity), `extract` metadata flag (`none` skips extraction entirely; `durable-only` injects a second system message at reflect time; absent / `all` / unknown extract as today via back-compat fallthrough), `n_extractor_failures` column on `reflector_runs` (migration 0004) so operators can distinguish "no facts found" from "extractor unreachable" via SQL alone, and `(S, P, O)` trigram (lexical scoring now consults `statement || subject || predicate || object` via `word_similarity`; switched from symmetric `similarity` because short queries against long concatenated text scored too low under the previous threshold). Test count 229 → 235 (+7 new: 2 storage, 4 reflect, 1 finish_run extension; net counting an updated existing `finish_run_sets_finished_at` test as carried-forward). All `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` green. Phase A's "Done means" lines (one per item) are encoded as the regression-test set above; operator-driven re-extraction of the dogfood corpus is the next checkpoint.

- **2026-05-14** — Pre-M3 design pass landed. Decisions captured in the table above; m3-search-quality.md gained `## Phase plan`, S/M/L tags, item-level "Done means…" lines, and a resolved retraction-durability item. See `docs/milestones/m3-search-quality.md` commit `6d3623b`.
