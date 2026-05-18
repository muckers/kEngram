# M6 — Corpus stats CLI + tagger-extracted relations (v1, non-thought targets)

**Status:** shipped 2026-05-17.

**One-line:** an operator-facing `engram stats` CLI subcommand for corpus + storage telemetry, plus the tagger's first auto-emission of relational edges (URLs, entities, persons mentioned in prose) to land directly in `thought_links` with `source = 'tagger'`.

## Design pivot

The original M6 milestone was **artifacts** — long-form document ingestion with `artifact_chunks` populated, chunking strategy, `ingest_artifact` MCP tool, and unified search across thoughts + chunks. Three signals shifted the plan:

1. The M5.2 `to_url` link target already covers the "this thought references that external doc" case without ingesting the document. Most operator needs were satisfied.
2. A 2026-05-17 live-corpus measurement (12 MB across 41 thoughts; ~1.5 KB user-data-per-thought) showed engram occupies a high-signal-density "sweet spot" between transcripts (byte-heavy) and tags (information-lean). Storing arbitrary long-form documents would dilute that.
3. Two more pressing needs surfaced: operators couldn't ask the corpus "how big are you?" without psql, and the **tagger-extracted relations** capability (LLM emits edges from prose) had a low-cost v1 shape thanks to M5.2's polymorphic targets.

The artifacts plan is preserved in `m6-artifacts.md` for historical reference. M6 reshaped to the present scope; M7 (operational maturity) unchanged.

## Scope

### M6.0 — `engram stats` CLI subcommand

- New top-level CLI subcommand: `engram stats [--scope-prefix X] [--top-scopes N]`.
- New storage helper `corpus_stats(pool, scope_prefix) -> CorpusStats` aggregating thought counts (live/retracted/untagged), content/tags/metadata byte totals, embeddings by model, link counts (by relation / by_kind / by_source), queue depths (pending_embeddings + new `pending_tags`), per-scope summary (reuses `list_scopes`), per-table heap/index/total sizes (via runtime-checked query against `pg_class`/`pg_relation_size`), and database total size.
- Plain-println rendering matching the `engram audit migrations` style; no new table-printing dependency. Sizes via `humanize_bytes` helper (1 KB = 1024 B; matches `pg_size_pretty` framing).
- No MCP surface in v1 — Ron's stated preference is operator-only ("more for me to track operational constraints without accessing the DB directly").

### M6.1 — Tagger-extracted relations v1

- `Tags` struct gains `relations: Vec<ExtractedRelation>` field (serde-default empty for backward compat with v1-v4 tags).
- New `ExtractedRelation { relation: RelationKind, target: ExtractedTarget, note: Option<String> }` and `ExtractedTarget` enum (`Entity | Person | Url` — no `Thought` variant in v1; thought-target tagger relations are deferred until entity resolution lands).
- v5 tagger prompt + JSON schema: Relations section explains the closed 7-relation vocabulary, the three target kinds, selectivity rules ("default to []", "require an explicit relational claim"), anti-patterns ("mere mention is not a relation"). Schema enforces `maxItems: 5`, closed enums on `relation` and `to_kind`, validates `to_value` length.
- `BUNDLED_TAGGER_VERSION: 4 → 5`. `engram tag --rerun --since 1970-01-01T00:00:00Z` re-tags v4 thoughts under v5.
- Drainer-side wiring (`engram_mcp::apply_tagger_relations`): after `update_thought_tags`, soft-delete prior `source='tagger'` edges from the thought (preserves audit trail; preserves `source='agent'` edges), then `insert_link` each emission with `source = 'tagger'`. Validates each target via `link::validate_target` at the same gate the agent-side `link_thoughts` uses. Bypass-on-error: a single malformed emission (e.g., non-`http(s)://` URL) is logged and skipped, never fails the whole tag job.
- New storage helper `soft_delete_tagger_edges_for_thought(pool, thought_id) -> i64`.
- `link::validate_target` visibility: `fn` → `pub(crate) fn` so the drainer reuses the same validation.
- `run_tag` CLI mirrors the drainer's relation-emission loop for synchronous re-tag runs.

## Decision log

- **CLI-only stats v1.** Operator preference; MCP `stats` tool deferred. The storage helper (`corpus_stats`) is reusable, so a future MCP wrapper is ~50 LOC. Re-evaluate if dogfood reveals agents wanting the data in-conversation.
- **`engram stats` is a top-level subcommand, not under `audit`.** The `audit` namespace is for log-table queries (migration_audit); stats is a live operator query. Different shape, different name.
- **Non-thought targets only for v1 tagger relations.** Thought-target extraction requires entity resolution ("which thought is the earlier finding?"), substantial design surface. Shipping non-thought targets first validates whether tagger-emitted edges feel right in dogfood before paying the resolution cost.
- **Soft-delete-then-insert on re-tag.** Re-tagging a thought soft-deletes its prior tagger-emitted edges and inserts fresh ones. Preserves audit trail (operator can see what v4 emitted via `deleted_at`); no accumulation if prompt drifts; mirrors M5.2's pattern. Agent-supplied edges (`source='agent'`) are unaffected.
- **Bypass-on-error in the drainer.** A malformed individual emission (failed validation, FK miss, etc.) is logged and skipped; the rest of the relations and the tag job itself proceed. Operators see warns; the corpus isn't blocked.
- **System-catalog query via `sqlx::query()` (runtime-checked).** `pg_class` / `pg_relation_size` can't be macro-checked. Matches the `insert_embedding` precedent for pgvector binds. Postgres-specific; called out in `corpus_stats`'s doc comment.
- **`maxItems: 5` on relations.** Caps per-thought tagger emission to keep responses small and force selectivity. Iterable in v6 if dogfood shows the cap is biting useful cases.

## Schema impact

No migrations. M5.2 already shipped:
- Polymorphic `thought_links` targets (entity / person / url) — used directly by tagger emissions.
- `LinkSource::Tagger` enum value (`source` column already allows it).
- Soft-delete (`deleted_at` + partial unique index) — used by `soft_delete_tagger_edges_for_thought`.

## MCP surface

- No new MCP tools.
- `link_source` field in `get_related_thoughts` responses now reliably returns `"tagger"` for tagger-emitted edges. (Operators / agents can distinguish them from agent-supplied edges via this discriminator.)

## CLI surface

- New `engram stats [--scope-prefix X] [--top-scopes N]`.
- New `engram audit migrations` (was M5.2; mentioned here for completeness alongside the new stats subcommand).
- `engram tag` and `engram embed-backfill` were extended in M5.2 with `--scope-prefix` flags.

## Tests added

- engram-storage: `corpus_stats_returns_aggregate_counts`, `corpus_stats_scope_prefix_filters_scopes_section_only`, `corpus_stats_table_sizes_include_thoughts_and_embeddings`, `corpus_stats_empty_corpus_returns_zeros`, `soft_delete_tagger_edges_for_thought_only_touches_tagger_source`, `soft_delete_tagger_edges_for_thought_idempotent_on_already_deleted`.
- engram-core: `tags_relations_serde_round_trip` (`extracted_relation_serde_round_trip`), `extracted_relation_note_optional`, `extracted_target_into_link_target_preserves_kind_and_value`, `v4_shape_without_relations_deserializes_with_empty_relations`.
- engram-extract: `valid_response_with_relations_parses_to_tags`, `tags_response_format_includes_relations_array`, plus v4→v5 prompt regression rename with new assertions on the Relations section.
- engram-mcp drain: `drain_tags_inserts_emitted_relations_with_source_tagger`, `drain_tags_re_run_soft_deletes_prior_tagger_edges_then_inserts_fresh`, `drain_tags_preserves_agent_edges_during_retag`, `drain_tags_skips_invalid_target_continues_others`.
- engram-cli: `humanize_bytes_renders_unit_scale`.

334 total tests passing post-M6.

## Out of scope (deferred)

- **MCP `stats` tool.** Operator can revisit if dogfood reveals agents wanting in-conversation corpus telemetry.
- **Thought-target tagger relations.** v2 work. Requires entity resolution (heuristic + LLM disambiguation against recent same-scope thoughts).
- **First-class entity / person tables.** Entities/persons remain free-text strings on `thought_links.to_entity` / `to_person`.
- **Tagger relation confidence scoring.** v1 emits-or-doesn't; threshold-filtering can land later if dogfood shows noisy emissions.
- **`engram stats --json`.** Plain-text only for v1.
- **Original M6 (artifacts).** Permanently dropped. The M5.2 `to_url` link target covers the common "reference external doc" use case; storing arbitrary documents was the wrong shape for engram's signal-density corpus.
- **Hard-purge of soft-deleted tagger edges.** Backlogged. Pair with a retention-policy CLI subcommand when growth becomes interesting.

## Risks

- **Tagger prompt v5 quality is empirical.** The wiring is straightforward; whether the prompt produces useful relations vs. noisy ones is a dogfood question. Same pattern as M4.1's v2→v3→v4 prompt iteration — ship a deliberately selective starting point, iterate.
- **Tagger latency increase.** Adding a `relations` field to the LLM response is a minor extension of the same JSON call. Should be small; the schema-constrained mode keeps inference bounded.
- **Re-tag churn rows.** Each `--rerun` soft-deletes the prior tagger edges and inserts fresh ones. At single-operator scale this is trivial; flagged for M7 if storage growth becomes operationally interesting.
