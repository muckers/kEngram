# M8 — Progress

Living checklist tracking M8 implementation. Each phase ends in a runnable, reviewable
checkpoint. Items are checked off as they land; the **History** section at the bottom captures
dated notes — decisions made in passing, surprises, things deferred. The companion design doc is
`m8-human-read-surface.md` in this directory.

## Phase 0 — Docs + scaffold ✅

End state: M8 docs exist; the v0 "no web UI" non-goal is reversed across the docs; an empty
`kengram-web` crate compiles in the workspace; `[web].enabled` config (default false) round-trips.

- [x] `docs/milestones/m8-human-read-surface.md` (this milestone's design doc)
- [x] `docs/milestones/m8-progress.md` (this file)
- [x] Reverse the non-goal in `CLAUDE.md`, `DESIGN.md`, and `docs/milestones/m7-operational-maturity.md`
- [x] DESIGN.md revision-history entry for M8
- [x] `crates/kengram-web` crate skeleton (`Cargo.toml` + empty `src/lib.rs`); added to root
  `Cargo.toml` `members` + `[workspace.dependencies]`
- [x] `WebConfig { enabled: bool }` on `Config` in `kengram-cli/src/config.rs` (default false)
- [x] Config round-trip test (`web_enabled_round_trips_from_toml` + `web_surface_disabled_by_default`)
- [x] `cargo build --workspace` clean

## Phase 1 — Serialization seam (kengram-mcp) ✅

End state: the five response→JSON mapper functions are `pub` and reusable from another crate; the
`/mcp` wire output is unchanged.

- [x] Relocate `search_response_json`, `recent_response_json`, `list_scopes_response_json`,
  `get_thought_response_json` to `search.rs`; `related_thoughts_response_json` to `relate.rs`
- [x] Mark `pub`; re-export from `kengram-mcp/src/lib.rs`
- [x] Fix `server.rs` call sites (imported the moved fns; dropped now-unused response-type imports)
- [x] `cargo test -p kengram-mcp` green — 131 passed, incl. the exact-JSON-shape tests
  (`search_thoughts_tool_response_carries_tags_per_hit`, `search_response_omits_score_field`)

## Phase 2 — Read-only JSON API ✅

End state: the five `/api/*` endpoints return live JSON identical to the MCP tools; mounted behind
the flag; unit-tested.

- [x] `WebState { pool, embedder, reranker, allowed_hosts }` + `router(WebState)` (model read via
  `embedder.model()`, mirroring the MCP server — no separate field needed)
- [x] `error.rs` HTTP status mapping (404/400/500; embedder-down is a soft-fail, not an error)
- [x] `host_guard.rs` middleware (mirrors rmcp `allowed_hosts`; matches bare-host + host:port + IPv6)
- [x] `GET /api/search`, `/api/recent`, `/api/scopes`, `/api/thoughts/{id}`, `/api/thoughts/{id}/related`
- [x] Mount web router in `kengram-cli::run_serve` behind `[web].enabled`
- [x] Router unit tests (`tower::ServiceExt::oneshot`): 200/JSON, 404, 400, 403 — 8 green
- [x] Live curl against the real corpus: search returns `vector_search_available:true` +
  `rerank_used:true` + reranked hits; scopes/thought/related all 200; bad Host → 403; bad param → 400

## Phase 3 — SSR shells + assets ⏳

End state: search, thought-detail, and scope-browser surfaces render in a browser.

- [ ] askama `base.html` + per-page templates
- [ ] `rust-embed` static handler (`GET /static/{*path}`) + `app.css`
- [ ] `/` search page + vanilla `app.js` search interaction
- [ ] `/thought/:id` detail page (hydrates from `/api/thoughts/:id` + `/related`)
- [ ] `/scopes` + `/scope/:name` (server-rendered, no-JS fallback)

## Phase 4 — Graph visualization ⏳

End state: the `/graph` view loads a root thought and expands the link graph on click.

- [ ] Vendor `cytoscape.min.js` into `static/`
- [ ] `/graph` + `/graph?root=:id` page
- [ ] Click-to-expand over `/api/thoughts/:id/related`; dedupe/merge; track expanded set
- [ ] Style nodes/edges by relation + target kind (thought/entity/person/url)

## Phase 5 — Polish + end-to-end ⏳

End state: M8 success criteria met; browser end-to-end pass clean.

- [ ] Search-as-you-type debounce; embedder-down banner; score badges
- [ ] Loading / empty / error states; Cache-Control on static assets
- [ ] Read-only audit (no mutating imports, no write routes); flag-off 404 check
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all --check`
- [ ] Browser end-to-end: search → detail → graph expand → scope browse

## History

- **2026-06-30** — Phase 2 landed. New `kengram-web` crate: `WebState` + `router()`, five read-only
  `/api/*` handlers wired to the `kengram-mcp` orchestrators and serialized through the relocated
  canonical mappers (so `/api` JSON == `/mcp` JSON by construction), an HTTP error mapping
  (`ReadError`/`RelateError` → 400/404/500; embedder-down stays a soft-fail surfaced via
  `vector_search_available`, not a 503), and a Host-header guard mirroring rmcp's `allowed_hosts`.
  Mounted in `run_serve` behind `[web].enabled`. 8 crate tests green (2 host-guard unit + 6
  `sqlx::test` oneshot). Live smoke against the real corpus confirmed the full pipeline runs through
  `/api` (vector+trigram+RRF+rerank all active). No model field on `WebState` — `embedder.model()`
  suffices, matching the MCP server.
- **2026-06-30** — Phase 1 landed. Relocated the five response→JSON mapper fns out of
  `server.rs` (search-family → `search.rs`, related → `relate.rs`), made them `pub`, re-exported
  from `lib.rs`, and pointed `server.rs` at the imports (dropping the now-unused response-type
  imports). The whole point — `/mcp` wire output unchanged — is pinned by the 131 green
  kengram-mcp tests, including the exact-JSON-shape ones. Chose relocation (not
  `#[derive(Serialize)]`) so `/api` and `/mcp` share one source of truth and can't drift.
- **2026-06-30** — Phase 0 landed. Milestone + progress docs written; the v0 "no web UI"
  non-goal reversed in `DESIGN.md`/`CLAUDE.md`/`m7-operational-maturity.md` (narrowed to "read-only
  UI in scope; psql stays write/admin"). `kengram-web` crate scaffolded (9th member, compiles
  empty); `[web].enabled` config added (default false) with two round-trip tests.

- **2026-06-30** — M8 opened. Doc-driven kickoff: milestone + progress docs written; the v0
  "no web UI" non-goal reversed in `DESIGN.md`/`CLAUDE.md`/`m7-operational-maturity.md` (narrowed
  to "read-only UI is in scope; psql remains the write/admin interface"). Architecture decided
  with Ron: Rust SSR + vanilla JS (no Node), a read-only `/api/*` layer reusing the existing
  `kengram_mcp` orchestrators (shared core, not browser-speaks-MCP), all four surfaces in v1, no
  app auth in v1 (localhost + Caddy/Tailscale boundary). Prior exploration confirmed the
  orchestrators are `pub`/re-exported but their response types aren't `Serialize` — the reuse seam
  is the five existing private JSON-mapper fns in `server.rs` (Phase 1 promotes them to `pub`).
  Notable: zero schema migrations, zero MCP-tool changes.
