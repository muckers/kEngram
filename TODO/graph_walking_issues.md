# kengram Tool Surface — Feedback from a Visualization Exercise

## Context

This document captures friction points and proposed improvements observed while attempting a moderately ambitious task: building a visualization of the full thought-to-thought link graph across the kengram corpus. The exercise was deliberately chosen to stress-test the tool surface, since visualizations require enumerating edges, fetching tags in bulk, and identifying connectivity patterns — workloads the current MCP tools don't directly support.

The visualization itself succeeded (rendered 12 connected thoughts across 4 components in scopes `rjf.tech` and `engram.tagger-test`), but the path to get there exposed structural gaps. This document summarizes those gaps and proposes additions with the goal of making graph-shaped and corpus-shaped queries first-class operations.

## Summary

The current tool surface is well-designed for **single-thought workflows**: capture one thought, find one thought, link it to another, walk its immediate neighborhood. It does not currently support **corpus-level or graph-level workflows** without N+1 round-trips. The highest-leverage additions are:

1. A flat edge-enumeration query (`list_edges`)
2. Batch thought retrieval (`get_thoughts`)
3. Tags returned inline by `recent_thoughts`
4. A `list_orphans` or `has_edges` filter to separate connected thoughts from isolates

These four additions would have cut the visualization workflow from ~30 tool calls to ~3 and would benefit any downstream consumer building dashboards, audits, or graph-shaped agents.

## Friction encountered during the exercise

### 1. No graph-level edge enumeration

To discover edges in the corpus, the only path is:

```
list_scopes() →
  recent_thoughts(scope=X, limit=N) (one per scope) →
    get_related_thoughts(thought_id=Y) (one per thought)
```

For a corpus of ~110 thoughts across 14 scopes, that's 14 + ~110 = ~124 tool calls just to discover edges, the vast majority of which return empty (most thoughts are unlinked). Empty calls are pure cost with no signal.

In practice during the exercise: I walked 34 thoughts across two scopes and found edges on only 12 of them. ~22 calls produced no edges. That's a ~65% miss rate on what should be a graph-level enumeration.

**What's needed:** a flat query returning edges directly, filterable by scope, link source, and relation type.

```
list_edges(
  scope?: string,
  scope_prefix?: string,
  link_source?: "agent" | "tagger",
  relations?: ["refines", "references", ...],
  target_kinds?: ["thought", "entity", "person", "url"],
  limit?: int,
  cursor?: string
) → {
  edges: [{
    link_id, from_thought_id, relation, to_kind, to_value,
    link_created_at, link_source, note, retracted
  }, ...],
  next_cursor: string | null
}
```

This single call replaces the N+1 walk for any graph-shaped consumer.

### 2. No batch thought fetch

After identifying which thoughts are in the connected graph, the next step is to fetch their content + tags for display. The only path today is:

```
get_thought(thought_id=A) → ... → get_thought(thought_id=L)  // 12 calls
```

A `get_thoughts(thought_ids: [A, B, ..., L])` taking an array and returning an array would collapse this to 1 call. For visualizations rendering N nodes with their tags, this is the single biggest latency win available.

### 3. `recent_thoughts` does not return tags inline

`recent_thoughts` returns content + metadata + scope + source + thought_id but omits tags. To get tags, the consumer must follow up with `get_thought` per result (or use `search_thoughts`, which has its own constraints — see point 4).

This shape inconsistency is a recurring papercut. The fix is straightforward: add an optional `include_tags: bool` parameter (default `false` if backwards compatibility matters; `true` would be more ergonomic).

The current design forces every consumer that wants to display tagged results to either pay 2x round-trips or contort queries to use `search_thoughts` as a tag-fetcher. Neither is a good default.

### 4. The trigram-only fallback gates short or unnatural queries

`search_thoughts` is the only tool that returns tags inline, but it requires a query that clears the trigram-similarity threshold. Short queries (1-2 words) consistently return empty even when the literal string appears in target content. Observed during the exercise: queries like `"beast"` and `"mosh"` returned zero results despite matching content, forcing me to construct long keyword-bombed queries like `"ASUS ProArt X870E-Creator motherboard fan sensor lm-sensors Nuvoton Glances Open WebUI Ollama Docker Tailscale"` just to retrieve a known thought.

Two paths forward:

- **Lower or remove the trigram-similarity threshold when no embedder is available.** If trigram is the fallback when vector search is down, gating it on similarity defeats the purpose. A literal-substring fallback would be more useful than a trigram-similarity fallback in that mode.
- **Add a tag-only retrieval that bypasses search entirely.** `get_thought_tags(thought_ids: [...])` or having `recent_thoughts` include tags (per point 3) makes the tag-retrieval workflow not require `search_thoughts` at all.

### 5. `list_scopes` returns stale counts and timestamps

During the exercise, `list_scopes` reported `rjf.tech: 2 thoughts, last_activity 2026-05-23` when the actual state was 5 thoughts including one captured ~30 minutes prior. A subsequent `recent_thoughts` call revealed the up-to-date state immediately.

This is workable when scopes are used for partitioning, but it's a real problem for any UI displaying scope-level summaries — the displayed counts will be wrong some fraction of the time. Either:

- Document the staleness window explicitly so consumers know to refetch (e.g. "list_scopes is eventually consistent with a typical lag of X seconds")
- Make `list_scopes` invalidate on capture/retract within the same scope
- Add an optional `consistency: "strong"` parameter that forces a fresh aggregate

The middle option is probably the right answer if the aggregates are computed at write time rather than query time.

### 6. Inconsistent tag-field shapes between tools

- `get_thought` returns tags at `provenance.tags`
- `search_thoughts` returns tags at the result-item top level (`results[i].tags`)
- `recent_thoughts` does not return tags at all

This means a consumer abstracting over the three tools needs three different paths to get the same conceptual data. Pick one shape and use it everywhere. The most idiomatic choice is top-level `tags` on the result object, regardless of which retrieval tool produced it.

### 7. No filtering on `link_source` at query time

In the visualization exercise, the meaningful structural edges are those created by `link_thoughts` calls (`link_source: "agent"`). The tagger-source edges to extracted entities/persons/URLs are auto-generated and would dominate any naive graph rendering — they outnumber agent-source edges by roughly an order of magnitude on most thoughts.

`get_related_thoughts` returns both source types and the consumer has to filter client-side. Adding `link_source: "agent" | "tagger"` as a query parameter (also applicable to the proposed `list_edges`) lets the consumer get only what they need.

### 8. No orphan / isolated-thought detection

A common visualization question is "show me only thoughts that have at least one edge" or its inverse, "how many thoughts in this scope have no links?" There's no way to answer this without enumerating every thought and checking each individually.

Adding a simple aggregate query helps:

```
get_link_stats(scope?: string, scope_prefix?: string) → {
  total_thoughts: int,
  thoughts_with_outbound: int,
  thoughts_with_inbound: int,
  isolated: int,
  edges_by_relation: { refines: N, references: N, ... },
  edges_by_source: { agent: N, tagger: N }
}
```

This is one call. It tells you both how much linking work has been done and where the gaps are.

### 9. No connected-component query (lower priority)

A direct "give me the connected components in scope X" query would have made the visualization a one-shot operation. This is more specialized than `list_edges` though — a consumer can build connected components client-side once they have the flat edge list, so this is lower priority than the foundational primitives above.

## Proposed additions to the tool surface

### High leverage

1. **`list_edges`** — flat edge enumeration, the single highest-leverage addition. Replaces the N+1 walk pattern for all graph-shaped consumers.

2. **`get_thoughts` (batch)** — array-in, array-out variant of `get_thought`. Cuts node-fetching from N calls to 1.

3. **`recent_thoughts` returns tags inline** — either by default or via an `include_tags: bool` parameter. Removes the most common 2-step retrieval pattern.

4. **`get_link_stats`** — aggregate query for orphan/connected/edge counts. One call replaces an entire scope walk for any dashboard.

### Medium leverage

5. **Consistent `tags` field placement** across `get_thought`, `search_thoughts`, and `recent_thoughts`. Pick top-level `tags` and apply everywhere.

6. **`link_source` filter** on `get_related_thoughts` and `list_edges`. Lets consumers separate the structural agent-authored graph from the dense tagger-extracted satellite graph.

7. **`list_scopes` freshness** — either invalidate on capture/retract, or document the staleness window. Counts displayed to users should be reliable.

8. **Trigram fallback usability** — lower the similarity threshold or add a substring-match mode when the embedder is unavailable. Currently the fallback is effectively non-functional for short queries.

### Nice-to-have

9. **`get_connected_components(scope?)`** — direct connected-component enumeration. Lower priority since `list_edges` lets consumers compute this client-side, but eliminates the need for graph traversal code in every consumer.

10. **`list_thoughts(has_edges: true)`** — alternative to `list_edges` for cases where the consumer wants thoughts-with-some-link-of-any-kind without enumerating the edges themselves.

11. **`search_thoughts` with `tag_filter` returning thought IDs only** (lightweight mode) — for use cases where the consumer only needs to know which thoughts match a tag filter, not their content.

## What works well today

Worth calling out the design choices that hold up well — these should be preserved as the surface evolves:

**The `note` field on edges is high signal.** It's where the author explains *why* the relation exists, in their own words. The visualization exercise found that the agent-authored notes (e.g., *"Adds the chipset-x4 slot-3 path as a Phase 2 de-sandwich option (overlooked in the prior options enumeration)"*) carry more meaning than the relation type alone. Keeping this as free-text and surfacing it in all graph-walk responses is the right design.

**Retracted-but-preserved-in-graph thoughts.** The current behavior of keeping retracted thoughts as edge targets (with the `retracted: true` flag) preserves the historical narrative of "what was once claimed, what replaced it." The visualization exercise found two retracted thoughts (`0747d3c8`, `c8648059`) still serving as meaningful structural nodes. Audit-trail-by-default is the correct stance for a memory system.

**Closed relation vocabulary.** Seven well-chosen verbs (`refines`, `replaces`, `references`, `supports`, `belongs_to`, `decided_by`, `requires`) is a defensible set. The exercise observed only three actively used (`refines`, `replaces`, `references`), but that's likely an artifact of the corpus and the operator's working patterns rather than a vocabulary issue. The closed-set discipline is correct.

**Polymorphic edge targets.** Edges that can point at thoughts, entities, persons, or URLs (one of four target kinds) is genuinely useful — the visualization deliberately suppressed the non-thought edges to focus on structural relations, but the existence of those targets is what makes the corpus interlink with the broader world.

**The tag drainer auto-creating tagger-source edges.** While the visualization filtered these out for clarity, the underlying capability is valuable: every thought gets a free first-pass entity graph without operator effort. The fact that these edges are clearly marked (`link_source: "tagger"`) means consumers can filter them in or out at query time.

**`get_related_thoughts` returning both inbound and outbound in one call.** This is the right shape for graph-walking. The grouped response with target preview snippets means a single call gives the consumer enough context to decide whether to drill deeper, which is exactly the discovery-walk pattern the system anticipates.

## Suggested sequencing

If the additions above are implemented incrementally, the suggested order is:

1. **`recent_thoughts` returns tags inline** (1-line change, immediate ergonomic win)
2. **Consistent `tags` field placement** (refactor, no new behavior — clean up before adding more)
3. **`list_edges`** (new primitive — biggest leverage)
4. **`get_thoughts` (batch)** (new primitive — second-biggest leverage)
5. **`get_link_stats`** (aggregate query — depends on #3 conceptually)
6. **`link_source` filter parameter** (small change to existing tools, depends on #3)
7. **`list_scopes` freshness** (consistency fix, no API change)
8. **Trigram fallback usability** (separate optimization)
9. **Nice-to-have additions** (connected components, has_edges filter, lightweight tag-filter mode)

Items 1–4 form the foundation. After those four, the visualization exercise from this document would have taken roughly 3–4 tool calls instead of ~30, and would have been able to display full tag context for every node rather than the abbreviated labels in the current visualization.

## Closing notes

The current tool surface is well-designed for the workflows it was built around: capture, find, link, walk. The gaps surfaced by this exercise aren't design failures — they're the natural shape of a tool surface that grew up around single-thought operations and now needs to grow corpus-level and graph-level primitives.

The friction is real but the fixes are mostly additive rather than invasive. `list_edges` and `get_thoughts` slot in alongside existing tools without changing semantics. The tag-shape consistency is a refactor, not a redesign. The biggest win is probably also the smallest change: adding `include_tags` to `recent_thoughts`.

What's currently impossible is making the corpus *legible at a glance* — counts, components, orphans, edge distributions. That's a meaningful gap for any future dashboard or auditing tool, and closing it would also remove friction from the existing graph-shaped agent workflows that are increasingly being layered on top of kengram.

