# No first-class "force re-embed" — `embed-backfill --force`

## Context

While fact-checking the kEngram intro talk's claim that *"a model swap is a re-index, not a migration,"* a capability gap surfaced: there is **no first-class way to force a full re-embedding pass**. Tagging has `kengram tag --force` (re-tag every matching thought) and `--rerun` (re-tag where `tags_extractor_version` is behind), but embedding has neither. `embed-backfill` is **gap-fill only**.

The expectation — voiced by the operator — is symmetry: backfills *and* force-runs should exist for both derived signals. They don't, and it's worth recording why, and what to do about it.

## Current behaviour (evidence)

- `enqueue_unembedded_thoughts` only enqueues thoughts with **no** embedding row for the active model — `crates/kengram-storage/src/lib.rs:641`:
  ```sql
  LEFT JOIN embeddings e
      ON e.target_kind = 'thought' AND e.target_id = t.id AND e.model_id = $1
  WHERE e.id IS NULL
  ```
- The insert is non-overwriting — `crates/kengram-storage/src/lib.rs:175`:
  ```sql
  ON CONFLICT (target_kind, target_id, model_id, model_version) DO NOTHING
  ```
- Embedding `model_version` is **hardcoded to 1** (`.bind(1_i32)`, `lib.rs:181`); `EmbedderConfig` has no `model_version` field (`crates/kengram-cli/src/config.rs:97`). So there is no version lever either.
- `embed-backfill` CLI takes only `--scope` / `--scope-prefix` / `--limit` — `crates/kengram-cli/src/main.rs:58`. No `--force`.

**Net:** re-embedding is triggered *solely* by configuring a different `model_id`. Under the same id, backfill is a no-op and the insert refuses to overwrite. The only path today to recompute in place is a manual `DELETE FROM embeddings WHERE model_id = '…'` in psql, then `embed-backfill`.

## Why the asymmetry is *partly* principled

Tags are a function of **model AND prompt**; an embedding is a function of **model alone**.

- The prompt can change under a fixed `model_id`, leaving stale tag rows that look current — which is the entire reason tags carry `tags_extractor_version` and need `--rerun`/`--force`. See `DESIGN.md` §10, the `tag --force` commit `83f8620`, and the v14 provenance-binding comment at `crates/kengram-extract/src/openai_compatible.rs:269`.
- An embedding is deterministic in the model (same model + content → same vector), so the design treats *"a row exists for this `model_id`"* as *"it is correct,"* and force-reembed was deemed unnecessary.

To that extent the divergence in re-run semantics is reasoned, not accidental.

## Why it is *also* an unfinished surface

1. **The schema already reserves the version axis.** `migrations/0001` keys embeddings on `(target_kind, target_id, model_id, model_version)` — exactly like tags — but the config/CLI never wired `model_version` up (it is pinned to 1).
2. **`embed-backfill` was scoped as recovery, not as the re-embed primitive.** Its header (`crates/kengram-mcp/src/backfill.rs:1`) describes it as an *"operator escape hatch for healing the embedding state"* for two failure modes (pre-queue thoughts; enqueue lost a crash race) — "heal then drain," not "re-process."
3. **The determinism assumption has real exceptions** where same-model re-embed is legitimately needed but unsupported:
   - **Corrupt / partial vectors** from an embedder outage. We have hit embedder-eviction timeouts (kengram thought `80c24216`); a half-written or wrong-dim vector becomes a permanent "exists, so it's fine" row.
   - **Silent upstream model drift** — `bge-m3` weights change while the config id stays `bge-m3:1024`, so the id no longer uniquely determines the vector.
   - **Cleanup after a failed re-embed pass.**

## Proposed fix (small)

Add `embed-backfill --force` (and/or a dedicated `kengram reembed --force`) that:

- enqueues already-embedded thoughts for the active model (drop / relax the `WHERE e.id IS NULL` guard under `--force`), and
- inserts with `ON CONFLICT (target_kind, target_id, model_id, model_version) DO UPDATE SET vector = EXCLUDED.vector` so the recompute actually lands.

Keep it bounded by the existing `--scope` / `--scope-prefix` / `--limit`, mirroring `tag --force`. Optional: a `--snapshot` of the prior vectors for rollback, as `tag` does.

**Milestone:** candidate for M7 (operational maturity) — see `docs/milestones/m7-operational-maturity.md`.

## Origin

Surfaced 2026-06-23 while verifying deck claims against the code. Companion kengram thought: `892efe1f` (scope `project.kengram`).
