# M3 — Search quality

## Goal

Improve retrieval quality with a cross-encoder rerank pass. Search results that look "almost right" become "the right one is in the top three." Quality is a step-change without a change to the MCP search tool's contract.

This is the smallest of the five milestones in scope and complexity, deliberately. It's a quality bump, not new capability.

## In scope

- A cross-encoder reranker (default: BGE-reranker-v2-m3 or comparable) running in TEI's rerank-task mode — either as a second TEI instance or as a multi-task TEI deployment (TBD).
- Rerank stage in the search pipeline: retrieve top-K (default 50) via RRF fusion, rerank with the cross-encoder to top-N (default 10).
- Configurable per-call: `rerank: bool` (default `true`), `candidate_pool: int` (default 50).
- Both `search_thoughts` and `search_facts` (from M2) gain rerank support.
- Eval-suite-style A/B comparison harness (small, ad-hoc; the full eval suite lands at M5) used to validate that rerank actually helps on a fixture corpus.

## Out of scope (deferred to which milestone)

- Artifact-chunk search → **M4**
- Personalization, learned-rank → indefinitely (post-M5; probably never for single-user)
- Auth, observability, eval-suite formalization → **M5**

## Schema impact

None. Reranker is a runtime concern; no tables added or changed.

## MCP surface delta

- `search_thoughts(..., rerank?: bool, candidate_pool?: int)` — both fields optional with defaults; existing M1 callers continue to work unchanged.
- `search_facts(..., rerank?: bool, candidate_pool?: int)` — same, for M2's facts search.

## Crate structure delta

- **`engram-embed`** (most likely) gains a `Reranker` trait and a `TeiReranker` implementation. Alternative: a separate `engram-rerank` crate. To be decided in M3 planning based on whether reranker shares HTTP-client infrastructure with the embedder.
- **`engram-mcp`** updates the two search tool handlers to call the reranker after RRF fusion.

## Dependencies

- **Prior milestones:** M1 (search), M2 (`search_facts`).
- **External services:** TEI configured with a rerank-task model loaded. May be the same TEI instance as the embedder if running in multi-task mode, or a second instance.

## Success criteria

1. **A/B quality:** on a fixture set of ~50 query/expected-result pairs (drawn from the operator's actual captured thoughts), reranked nDCG@10 is materially higher than RRF-only nDCG@10. "Materially" = a difference the operator can feel in daily use; we'll define a numerical threshold during M3 planning.
2. **Latency:** rerank stage adds < 200 ms P95 to search latency on the operator's hardware (CPU TEI on the 9800X3D or GPU TEI on the 3090 — depends on deployment choice).
3. **Backward-compatible default:** clients calling `search_thoughts` with no rerank parameter get reranked results by default; existing M1/M2 client code continues to work.
4. **Operator dogfood:** the operator runs M3 for at least a week and reports whether rerank "feels worth the latency." If no, the default flips to `rerank: false` and we re-evaluate.

## Open questions

- **TEI deployment shape.** One TEI instance running both embed and rerank tasks, or two instances (one per task). Hinges on TEI's multi-task support maturity at the time of implementation.
- **Default candidate pool.** 50 is a reasonable default; should it be 100 for quality, 25 for latency? Per-tool defaults?
- **Rerank cutoff.** If reranker confidence is below some threshold across all candidates, should we report "low confidence overall"? Or just always return top-N?
- **Default-on vs. default-off.** The operator's preference matters here; default-on is the recommendation, but default-off may be safer initially.
- **Reranker model choice.** BGE-reranker-v2-m3 is the obvious default; bake-off against alternatives is an M5 eval-suite job, not M3.
- **Scope prefix filtering.** Today `scope` is exact-match only (confirmed empirically on 2026-05-12 during the M1 smoke test). The dotted-scope convention in §214 of the design doc is purely human-readable — `work.tcgplayer.platform.pricing` is not findable when filtering by `work.tcgplayer`. The operator has adopted a "flat-and-few" scope convention to live with the constraint. Open: after a week+ of M1 dogfood, is the lack of prefix filtering actually painful? If yes, add `WHERE scope = $1 OR scope LIKE $1 || '.%'` to the storage layer (one-line change, no schema impact). If no — i.e. the discipline of flat scopes is doing useful work — leave it alone. Decide with dogfood evidence, not speculation.
