# M8 — Human read surface

## Goal

kEngram gets a browser surface a *human* can use to search and explore the corpus. The MCP
server is excellent for agents but useless at a keyboard; M8 adds a read-only web UI — search,
thought detail, an interactive link-graph view, and a scope browser — served by the same
`kengram` binary that already serves `/mcp`.

The design principle is **reuse, not reimplementation**: the existing transport-agnostic
orchestrator functions (`kengram_mcp::search`, `relate`) already carry the load-bearing
retrieval logic (RRF fusion, recency boost, JSONB tag-filter, cross-encoder rerank, one-hop
graph traversal). The web surface is a second *head* over that same core — a read-only
`/api/*` JSON layer plus server-rendered HTML — not a parallel implementation. This guarantees
the web API returns byte-identical JSON to the MCP tools.

This milestone **reverses the v0 "no web UI" non-goal** (`DESIGN.md`, `CLAUDE.md`,
`m7-operational-maturity.md`). The reversal is narrow: the UI is **read-only**; `psql` remains
the write/admin interface, and `/mcp` remains the agent interface.

## In scope

- **`kengram-web` library crate** (9th workspace member; `kengram-cli` stays the only binary).
  Houses the axum read-router, askama templates, embedded static assets, the host-guard
  middleware, and the HTTP error mapping.
- **Read-only JSON API** (`/api/*`), each endpoint reusing an existing orchestrator and its
  canonical JSON mapper:
  - `GET /api/search` → `search_thoughts` (hybrid vector+trigram, RRF, recency, tag-filter, rerank)
  - `GET /api/recent` → `recent_thoughts`
  - `GET /api/scopes` → `list_scopes`
  - `GET /api/thoughts/:id` → `get_thought` (with provenance)
  - `GET /api/thoughts/:id/related` → `get_related_thoughts` (one-hop; the graph-expansion endpoint)
- **Four SSR surfaces** (askama + vanilla JS):
  1. **Search** — query box → ranked results with scope, tags, and score breakdown.
  2. **Thought detail** — full content, provenance (embedding status, tagger model/version), tags,
     metadata, retraction state, related-edge list.
  3. **Interactive graph** — cytoscape canvas; click a thought to expand its one-hop neighborhood,
     keep expanding to walk the link graph (`refines`/`supports`/`references`/… and polymorphic
     `entity`/`person`/`url` targets).
  4. **Scope browser** — scopes with counts/activity; browse recent thoughts within a scope.
- **`[web]` config section** (`enabled: bool`, default `false`). `serve` mounts the UI only when
  enabled; otherwise `/` and `/api/*` 404 and only `/mcp` is served.
- **Host-header guard** on the new routes mirroring the rmcp `[server].allowed_hosts` check
  (rmcp enforces it only inside `/mcp`).
- **Single-binary deploy** — static assets (vendored `cytoscape.min.js`, hand-written `app.js`,
  `app.css`) are embedded via `rust-embed`; no separate asset directory to ship.

## Out of scope

- **Any write/mutation surface.** No `capture`/`link`/`unlink`/`retract` over HTTP. The UI is
  strictly read-only; mutations stay on `/mcp` (agents) and `psql` (operator). A future milestone
  can revisit operator-write surfaces once Tier-2 auth (M7) exists.
- **App-level auth.** v1 relies on the existing boundary: localhost bind + Caddy/Tailscale front
  door (single-user). Per-token scope isolation is the M7 Tier-2 story; when it lands, the web
  surface can adopt it.
- **Multi-hop graph traversal in the backend.** The storage layer offers one-hop only; the graph
  view expands one hop per click client-side. A batched multi-hop endpoint is a possible follow-up.
- **Node/npm/TypeScript toolchain.** Frontend is Rust SSR + vanilla JS + a vendored JS lib;
  consistent with the project's no-Node rule.
- **Full-text editing, saved searches, theming, mobile layout** — possible polish, not v1.

## Schema impact

**None.** No migrations. The web surface is pure read over existing tables (`thoughts`,
`embeddings`, `thought_links`). This is a notable property of the milestone.

## MCP surface delta

**None.** No new MCP tools; no signature changes. The only `kengram-mcp` change is internal:
five private JSON-mapper helper functions in `server.rs` are relocated to their owning modules
(`search.rs`/`relate.rs`) and made `pub` so the web layer can reuse them. The `/mcp` wire output
is byte-identical before and after (pinned by the existing exact-JSON-shape tests).

## Crate structure delta

- **New `kengram-web`** library crate: `router(WebState) -> axum::Router`, route handlers, askama
  templates, embedded static assets, host-guard middleware, HTTP error mapping. Depends on
  `kengram-core`, `kengram-mcp`, `kengram-storage`; new external deps `askama`, `rust-embed`,
  `mime_guess` (all pure-Rust).
- **`kengram-mcp`** — the five response-JSON mapper fns become `pub` and move next to their types;
  re-exported from `lib.rs`.
- **`kengram-cli`** — `run_serve` merges the web router when `[web].enabled`; `config.rs` gains
  `WebConfig`.

## Dependencies

- **Prior milestones:** M1 (capture/search core + the axum `serve` host), M3 (hybrid retrieval +
  rerank — what the search surface renders), M5/M6.1 (the `thought_links` graph — what the graph
  view renders). No dependency on the unshipped M7 auth work (the UI ships behind the existing
  network boundary).

## Success criteria

- With `[web].enabled = true`, `kengram serve` serves all four surfaces; with it absent/false,
  `/` and `/api/*` 404 while `/mcp` is unaffected.
- Each `/api/*` endpoint returns the **same JSON** as the corresponding MCP tool for the same
  inputs (verified by diffing live responses).
- Search renders ranked results and degrades gracefully when the embedder is down
  (`vector_search_available:false` → results from the trigram leg + a UI banner).
- The graph view loads a root thought and expands neighbors on click, walking the link graph.
- Read-only is enforced structurally: no mutating orchestrator is imported and no
  POST/PUT/DELETE/PATCH route exists.
- `cargo build`/`clippy -D warnings`/`fmt --check` clean; `cargo test -p kengram-mcp` green
  (wire shape preserved); `kengram-web` router tests green.

## Open questions

- Should the graph view eventually get a batched multi-hop endpoint, or is click-to-expand
  sufficient? (Deferred — start with one-hop.)
- When Tier-2 auth lands, does the web surface get its own token, or share the operator's? (Defer
  to the M7 auth conversation.)
