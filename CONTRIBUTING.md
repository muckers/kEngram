# Contributing to kEngram

Thanks for your interest in kEngram. Contributions are welcome — this guide
keeps them reviewable and keeps the project coherent. Please read it before
opening a pull request; it will save us both a round trip.

kEngram is a single-user, self-hosted, MCP-native memory service with a
deliberately small surface and a written terminal-state design. The bar for
changes is therefore "does this fit the design and land cleanly," not "is the
code good in isolation."

## Before you write code

- **Read [`DESIGN.md`](DESIGN.md) first.** It describes the terminal system and
  the rationale behind the choices below. Per-milestone scope lives in
  [`docs/milestones/`](docs/milestones/).
- **Open an issue for anything non-trivial.** Architecture or behavior changes —
  a new retrieval leg, a new embedding dimensionality, a schema change, a new MCP
  tool — should start as a **design proposal** (an issue, or a short `docs/` note)
  that references the relevant `DESIGN.md` section. Get directional agreement
  *before* writing the implementation. A wrong guess on direction is expensive to
  unwind, so we'd rather discuss the shape up front.
- Small, obvious fixes (typos, docs, a clearly-correct bug fix) can skip straight
  to a PR.

## Pull request expectations

- **One concern per PR.** A PR should do one reviewable thing. Bundling an
  embedder, a lexical-search change, and a schema migration into one branch makes
  it un-reviewable and un-mergeable — split them.
- **Branch from current `main`.** Don't base a PR on a private fork's baseline or
  carry unrelated commits. The diff should contain only your change.
- **Migrations are append-only and immutable.** Never edit a migration that has
  already shipped (`migrations/0001_initial.sql` and friends) — existing
  deployments validate migration checksums and will break. Add a new, sequentially
  numbered migration instead. A model/index change is additive: a new partial
  index or column in a new migration, never an edit to an old one. See
  [`DESIGN.md`](DESIGN.md) §10 (thoughts are immutable) and the migration audit
  conventions in [`DEVELOPMENT.md`](DEVELOPMENT.md).
- **Respect the fixed stack.** Rust + Tokio + axum + Postgres (pgvector + pg_trgm)
  + sqlx. **No Python, Node, or TypeScript dependencies.** External LLM/embedding
  services (TEI, vLLM, etc.) are *consumed* over HTTP — kEngram does not operate
  them.
- **Swappable backends go through the traits.** New embedding / tagging /
  reranking providers implement `Embedder` / `Tagger` / `Reranker` and are opt-in
  via config — not hardwired into the pipeline.
- **No new MCP tools without a design discussion.** The full tool set is governed
  by the design doc and ships by milestone.

## Quality gates (must pass before review)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # add --features integration if you touched storage; needs Postgres
```

- No `unwrap()` outside tests; use `expect("why")` only where you genuinely can't
  recover.
- Use compile-time-checked `sqlx::query!` / `query_as!` for SQL.
- Put tests next to the code they cover.

Commit messages follow Conventional Commits (`feat:`, `fix:`, `docs:`,
`chore:`, …), matching the existing history.

## Licensing

By contributing, you agree that your contributions are dual-licensed under the
[Apache-2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT) licenses, matching the
project.
