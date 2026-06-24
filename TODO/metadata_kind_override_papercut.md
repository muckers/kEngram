# `metadata.kind` is a footgun — the override only honors `metadata.decision_type`

## Context

A capturing client can hint the thought's classification two ways, and they look interchangeable but aren't:

- `metadata.kind` — a free-form, agent-supplied value stored verbatim in the metadata JSONB. **Nothing reads it.** It is opaque to retrieval, to graph traversal, and to the tagger.
- `metadata.decision_type` — the **only** metadata key that influences the tag `kind`. The finalize seam's `apply_decision_type_override` (`crates/kengram-mcp/src/filters.rs:70`) forces `tags.kind = decision_record` when `metadata.decision_type` is a non-empty string.

The footgun: an agent naturally reaches for `metadata.kind` (it mirrors the tag field name `tags.kind`) expecting it to set the classification. It does nothing. The tagger — which never sees metadata — independently classifies from prose, and its guess stands.

## Observed instance

Thought `892efe1f` was captured with `metadata.kind = "decision_record"`, but the tagger (`gemma4:31b-qat-ctx16k`, v16) classified the prose as `idea`, and the override never fired because the key was `kind`, not `decision_type`. Re-captured as `5a2cf300` with `metadata.decision_type` set; the original was retracted. (idea-vs-decision_record is also the tagger's dominant confusion class, so even without the key mismatch this divergence is common.)

## Why it bites

1. **Name collision invites the wrong key.** `tags.kind` and `metadata.kind` share a name but are unrelated namespaces; the bridge key is `decision_type`, which an agent would not guess.
2. **Silent.** A wrong-key hint produces no error and no warning — the thought just keeps the tagger's guess. Because the corpus has no review queue, a mis-`kind`ed thought looks authoritative.
3. **Immutable to fix.** Thoughts are immutable and the fingerprint unique constraint is full (not partial on `retracted_at`, `migrations/0006:30`), so correcting it requires retract + re-capture with *changed* content (identical bytes dedupe back to the retracted row). A metadata-only key typo is expensive to undo.

## Options

1. **Honor `metadata.kind` as an alias** in the finalize override — accept a closed-enum `metadata.kind` value (e.g. `decision_record`, `task`, `idea`, …) and force `tags.kind` to it, generalizing the current decision_record-only override. Most ergonomic; widens the override from one kind to the whole enum (decide whether that is wanted — the current narrow override was deliberate).
2. **Document loudly** in the `capture` MCP tool description and DESIGN: "to influence `tags.kind`, set `metadata.decision_type`; `metadata.kind` is ignored." Cheapest; keeps the narrow override.
3. **Warn on a likely-mistaken key** — if `metadata.kind` is present and `metadata.decision_type` is absent, surface a one-line note in the capture response. Catches the typo at write time.

Recommendation: at minimum (2); ideally (1) if generalizing the kind override is acceptable, since it removes the trap rather than papering over it.

## Origin

Surfaced 2026-06-24 while diagnosing why a captured decision record tagged as `idea`. Companion kengram thoughts: `5a2cf300` (the corrected record), `892efe1f` (retracted original).
