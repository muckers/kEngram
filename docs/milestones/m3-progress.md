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

**Dogfood validation (2026-05-14):**

- [x] Re-extract the dogfood corpus via `engram reflect --rerun` against v3 prompt + `qwen3-coder:30b`. **Partial pass.**
  - ✓ `commit_or_supersede` pipeline working as designed: 54 commits over 11 thoughts under the v3 prompt, dedup-via-supersession folding statement-matched drift; review-queue routed 1 fact (confidence rubric no longer uniformly anchoring at 0.85).
  - ⚠ SPO rules land inconsistently with `qwen3-coder:30b`. Comparative inversion still present on most affected fact_ids; self-referential triples still being emitted (5 new instances on the WebSockets thought alone). Some atomic-claim emissions DO follow the new rules (e.g. fact `2472dc0c`: S=Bazel/O=Make for "Bazel is more powerful than Make"), but the model isn't reliable.
  - 🐞 **New failure mode** surfaced: within a single extraction call the LLM is emitting two facts with byte-identical statements but different SPO decompositions (one per clause of a compound statement). `commit_or_supersede` folds them via the statement-match predicate, picking the *last* emission as canonical — which is non-deterministic on correctness. Documented as a new M3 backlog item ("Quality-aware dedup for within-call duplicates") under `## In scope > Pipeline quality`.
  - **Conclusion:** v3 prompt + dedup pipeline are correctly *in the binary* (Phase A code is closed), but v3-prompt effectiveness under `qwen3-coder:30b` is mediocre. A v3.1 prompt iteration and/or a quality-aware dedup pass would help; Phase B's A/B harness is the right tool for measuring this objectively across models. Not a Phase A blocker — the failure modes are now well-characterized and on the backlog.

**Side findings during dogfood (2026-05-14):**

- 🐞 `map_send_error` hardcoded `seconds: 60` in the timeout error display; fixed in commit `1d627e4` to report the actual configured value.
- 🐞 Extractor startup `tracing::info!` only logged the system prompt source; expanded in commit `1d627e4` to also log `model_name`, `model_version`, `timeout_seconds` so config-merge results are visible without per-fact debugging.
- 🔬 Tried `qwen3.5:35b-a3b-coding-nvfp4` as a more-capable alternative; nvfp4 quantization is NVIDIA-Blackwell-specific and falls back to CPU on Apple Silicon, producing 180s+ per-extraction timeouts. Reverted to `qwen3-coder:30b`. Metal-friendly counterpart `qwen3.5:35b-a3b-q4_K_M` exists (24 GB) but model comparison is properly Phase B A/B-harness work, not Phase A.

## Phase B — Retrieval quality

Three commits per Phase B plan (2026-05-14):

### Step 1 — Fact embeddings

End state: facts have embeddings flowing through the same async-embedding seam as thoughts; `search_facts` runs as real hybrid retrieval (vector + trigram fused via RRF).

- [x] `engram-storage`: `insert_fact_embedding` convenience wrapper.
- [x] `engram-storage`: `search_facts_vector_knn` mirroring `search_vector_knn` (joins facts + thoughts; cosine-distance ordered; filters active rows; per-model HNSW partial index already exists from migration 0001).
- [x] `engram-storage`: `enqueue_unembedded_facts` heal-side companion to `enqueue_unembedded_thoughts`.
- [x] `engram-mcp/drain`: `process_job` dispatches on `target_kind`: thoughts via the existing path, facts via `fetch_fact` + embed + `insert_fact_embedding`, anything else is `UnsupportedTargetKind` (preserves `artifact_chunk` future-proofing).
- [x] `engram-mcp/reflect`: `run_reflector_once` and `run_reflector_rerun` gain an `embedder_model_id: &str` parameter; `commit_or_supersede` plumbs it through and enqueues a `target_kind='fact'` row in `pending_embeddings` after every fact insert. No-op-floor branch does NOT enqueue (no new fact written → the byte-identical existing row was already enqueued at its original insert).
- [x] `engram-mcp/search`: `search_facts` gains `embedder: &dyn Embedder` parameter + a real vector leg + `vector_search_available: bool` on the response. Inline `rrf_fuse_facts` mirrors `engram-core::rrf_fuse` keyed on `fact.id`.
- [x] `engram-mcp/server`: `search_facts` tool handler passes `self.embedder.as_ref()` through; `search_facts_response_json` carries `vector_search_available`.
- [x] `engram-cli`: `embed-backfill` gains `--target {thoughts,facts,all}` flag, defaulting to `all`. `run_embed_backfill` plumbs through to `embed_backfill(target.into())`.
- [x] `engram-mcp::BackfillTarget` enum (engram-mcp-side; engram-cli has a clap-derived mirror with one-line `From` impl).
- [x] `cargo test --workspace`: 244 passing (235 today + 9 new).
- [x] `cargo clippy --all-targets -- -D warnings`: clean.

**Operator-driven (post-merge, verified 2026-05-15):**

- [x] `engram embed-backfill --target facts --limit 1000` against the live DB: **97 healed / 97 embedded / 0 failed**. Post-backfill DB state: `embeddings` table holds 97 fact rows + 13 thought rows; `facts WHERE superseded_at IS NULL` count = 97 (perfect 1:1 match).
- [x] MCP `search_facts` with a query that has zero token overlap with any fact's statement but is semantically related (probe 1, Claude Desktop): vector leg returned the semantically-related fact. **Step 1 success criterion met.**
- 🔬 Two quality-of-results observations from Claude Desktop's dogfood (*not step-1 blockers*; folded into M3 backlog):
  - **RRF score compression.** Top-of-list scores hover at 0.015–0.016 across both highly-relevant and weakly-related facts. This is design-correct RRF behavior: `score = Σ 1/(60 + rank_i)` discards absolute leg scores, so the *maximum possible* RRF score is `2/61 ≈ 0.033` for a doc top-1 in both legs. Consumer-facing implication captured as a new "surface per-leg scores in search response shape" backlog item in `m3-search-quality.md`.
  - **Probe 2 ranking anomaly.** Query "tooling for compiling codebases reproducibly" ranked Redis above Bazel; the Nix-reproducibility fact didn't surface. *Direct motivation* for Phase B step 2's cross-encoder reranker, which produces calibrated absolute relevance scores. Noted as a concrete regression target on step 2.

### Step 2 — Cross-encoder reranker + rerank stage

End state: TEI Docker container serves BGE-reranker-v2-m3; `search_thoughts` / `search_facts` retrieve top-`candidate_pool` via RRF + recency, rerank to top-`limit` via the cross-encoder; per-leg + rerank scores surface on every result; rerank is on by default with explicit per-call opt-out.

- [x] `docker-compose.yml`: new `tei` service (cpu-arm64 image, BAAI/bge-reranker-v2-m3, 60s start_period for the first-boot model download).
- [x] `engram-embed`: `Reranker` trait + `RerankerError` (with `is_transient()`) + `RerankScore`.
- [x] `engram-embed`: `TeiReranker` HTTP impl (POSTs `/rerank`; validates response shape; stores `timeout_seconds` so error reports the actual configured value per the Phase A lesson).
- [x] `engram-embed`: `FakeReranker` with `Deterministic` / `Timeout` / `Unreachable` / `Misconfigured` behaviors + `PositionDescending` / `PositionAscending` / `SubstringBoost` scoring strategies + `RecordedRerank` last-call inspection.
- [x] `engram-core`: `Hit` gains `vector_score`, `trigram_score`, `rerank_score` optional fields + `Hit::from_vector_leg` / `Hit::from_trigram_leg` constructors. `rrf_fuse` preserves per-leg scores across the fusion (Some wins over None).
- [x] `engram-storage`: `FactHit` gains the same three optional fields. `search_vector_knn` / `search_trigram` / `search_facts_vector_knn` / `search_facts_trigram` populate the right per-leg field at construction.
- [x] `engram-mcp/search`: `search_thoughts` and `search_facts` gain `reranker: Option<&dyn Reranker>` parameter + `rerank` / `candidate_pool` request fields + `rerank_used` on response. Inline `apply_rerank_to_thought_hits` / `apply_rerank_to_fact_hits` helpers feed the candidate pool into the cross-encoder, write `rerank_score` + mirror into `score`, re-sort. Soft-fail to RRF + recency order on reranker errors.
- [x] `engram-mcp/server`: `EngramServer` gains `reranker: Option<Arc<dyn Reranker>>` field. `SearchThoughtsArgs` / `SearchFactsArgs` gain `rerank` / `candidate_pool` schemars-documented optional fields. JSON serializers carry per-leg + rerank scores + `rerank_used`.
- [x] `engram-cli`: `RerankerConfig` (empty `provider` = silent disable sentinel). `build_reranker` returns `Option<Arc<dyn Reranker>>`. Startup log mirrors the extractor's Phase A pattern.
- [x] `DEVELOPMENT.md`: TEI section (first-time setup + smoke test) + `[reranker]` config example.
- [x] `cargo test --workspace`: 273 passing (244 → 273; +29 across reranker trait, integration, RRF per-leg preservation).
- [x] `cargo clippy --all-targets -- -D warnings`: clean.

**Operator-driven (post-merge):**

- [ ] `docker compose up -d tei` → confirm healthy.
- [ ] Add `[reranker] provider = "tei"` (etc.) to `~/.config/engram/engram.toml`.
- [ ] `engram serve` → startup log shows `reranker: resolved config`.
- [ ] MCP `search_facts` with rerank-on returns `rerank_used: true` and every hit carries a `rerank_score`.
- [ ] MCP `search_facts` with `rerank: false` returns the Phase B step 1 RRF + recency order; `rerank_used: false`; `rerank_score: null` on every hit.
- [ ] Regression-target probe: `tooling for compiling codebases reproducibly` → does the live BGE-reranker rank Nix-reproducibility facts above Redis? (Phase B step 3's A/B harness will measure this systematically; for now an eyeball check.)

### Step 3 — A/B benchmarking harness

Not yet planned.

- [ ] `engram bench rerank` CLI subcommand reading fixture file
- [ ] Curated fixture corpus at `tests/fixtures/rerank-bench.json` (~30-50 query/expected-hit pairs)
- [ ] nDCG@10 comparison table output

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

- **2026-05-15** — **M3 Phase B step 2 landed: cross-encoder reranker + per-leg scores.** TEI Docker container serves `BAAI/bge-reranker-v2-m3`; `search_thoughts` and `search_facts` retrieve top-50 (configurable `candidate_pool`) via RRF + recency, rerank to top-`limit` via the cross-encoder. Per-leg `vector_score` / `trigram_score` and `rerank_score` surface as optional fields on every result so consumers building threshold logic can reach the raw signals (the previous "score is opaque RRF rank-sum" feedback from the step 1 dogfood drove this bundling decision). Wire shape additive: `SearchRequest` / `SearchFactsRequest` gain `rerank?: bool` (default `true`) + `candidate_pool?: usize`; responses gain `rerank_used: bool`. New `[reranker]` config section with empty-provider sentinel for silent disable (Phase B step 1 dogfood deployments without a `[reranker]` block keep working unchanged). 17 new tests in engram-embed (TeiReranker via wiremock + FakeReranker variants + RerankerError classification), 6 new integration tests in engram-mcp/search (rerank on/off/no-reranker/soft-fail/reorder/per-leg-scores), 2 new RRF preservation tests in engram-core. Test count 244 → 273 (+29). TEI service confirmed healthy locally; live `/rerank` smoke returns the expected response shape. Operator-driven verification (rerank-on dogfood, regression-target probe) remains the next checkpoint.

- **2026-05-15** — **Phase B step 1 dogfood verification.** Claude Desktop ran qualitative probes against the live corpus (97 facts, freshly backfilled). Probe 1 verified the vector leg is doing real work: a statement-token-disjoint query returned the semantically-related fact — impossible via trigram. **Step 1 success criterion met; signing off.** Two observations folded into the M3 forward-looking backlog (`m3-search-quality.md`): (a) RRF score compression at 0.015–0.016 — design-correct, not a bug; added a new "surface per-leg scores in search response shape" backlog item for consumer thresholding; (b) probe 2 ranked Redis above Bazel for "tooling for compiling codebases reproducibly", with Nix-reproducibility missing entirely — annotated step 2 (cross-encoder reranker) with this as a concrete regression target.

- **2026-05-14** — **M3 Phase B step 1 landed: fact embeddings.** Closes M2 Phase D's deferred simplification — `search_facts` now runs as real hybrid retrieval (vector + trigram fused via RRF) instead of trigram-only. Three new storage primitives (`insert_fact_embedding`, `search_facts_vector_knn`, `enqueue_unembedded_facts`), drain-side fact dispatch in `process_job` (clean match on `target_kind` between thoughts and facts; `artifact_chunk` future-proofing preserved), reflector enqueues fact embeddings via the same `enqueue_embedding(target::FACT, ...)` pattern that `capture` uses for thoughts, `search_facts` gains an `embedder` parameter + `vector_search_available` response field + an inline fact-aware RRF fuse (kept `engram-core::rrf_fuse` Hit-specific since Phase B step 1 is the only fact-fusion site so far), and `embed-backfill --target {thoughts,facts,all}` lets operators heal pre-Phase-B facts on-demand. Test count 235 → 244 (+9 new: storage +3, drain +2, reflect +1, search +2, backfill +1). Build, test, clippy all green. No migration. Next: Phase B step 2 (cross-encoder reranker + TEI Docker + rerank stage in both search tools) — its own planning conversation.

- **2026-05-14** — **M3 Phase A closed out.** Validated Phase A via dogfood `engram reflect --rerun` against v3 prompt + `qwen3-coder:30b` (54 commits, 11 thoughts, 1 review-queue routing, 0 failures). Pipeline plumbing all works — v3 prompt content is being sent (logged `system_prompt=bundled`), `commit_or_supersede` is folding statement-matched drift into canonicals via supersession, the new review-queue routing fires when confidence drops below 0.7. Two operator-discovered bugs fixed in follow-up commit `1d627e4` (cosmetic hardcoded "60s" timeout display + missing config-resolution startup log). Detected limitations of v3 prompt under `qwen3-coder:30b`: SPO inversion / self-referential triples / compound-statement-multi-decomp not reliably suppressed. Documented in `## Pipeline quality` of m3-search-quality.md as a new "Quality-aware dedup for within-call duplicates" backlog item. v3-prompt-effectiveness across models is Phase B A/B-harness territory; Phase A's success criterion ("v3 prompt is in the binary, dedup pipeline works") is met. Brief detour to evaluate `qwen3.5:35b-a3b-coding-nvfp4` ran into 180s timeouts (nvfp4 isn't Metal-accelerated on Apple Silicon); reverted. Phase A is in the books.

- **2026-05-14** — **M3 Phase A landed.** Single bundled commit `M3 Phase A: pipeline-quality fixes` covering v3 prompt revision (SPO decomposition rules: comparative S=A/O=B, self-referential subject-MUST-NOT-equal-object, conditional-as-subject; tighter per-hedging-level confidence rubric; two new episodic-skip negatives; JSON envelope restated in prose; `model_version` 2 → 3), `commit_or_supersede` helper factored out of `run_reflector_rerun` and applied to `run_reflector_once` (within-call dedup-via-supersession parity), `extract` metadata flag (`none` skips extraction entirely; `durable-only` injects a second system message at reflect time; absent / `all` / unknown extract as today via back-compat fallthrough), `n_extractor_failures` column on `reflector_runs` (migration 0004) so operators can distinguish "no facts found" from "extractor unreachable" via SQL alone, and `(S, P, O)` trigram (lexical scoring now consults `statement || subject || predicate || object` via `word_similarity`; switched from symmetric `similarity` because short queries against long concatenated text scored too low under the previous threshold). Test count 229 → 235 (+7 new: 2 storage, 4 reflect, 1 finish_run extension; net counting an updated existing `finish_run_sets_finished_at` test as carried-forward). All `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` green. Phase A's "Done means" lines (one per item) are encoded as the regression-test set above; operator-driven re-extraction of the dogfood corpus is the next checkpoint.

- **2026-05-14** — Pre-M3 design pass landed. Decisions captured in the table above; m3-search-quality.md gained `## Phase plan`, S/M/L tags, item-level "Done means…" lines, and a resolved retraction-durability item. See `docs/milestones/m3-search-quality.md` commit `6d3623b`.
