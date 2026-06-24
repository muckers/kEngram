# kEngram intro talk

A short (6-slide) terminal deck introducing kEngram, built for an informal
engineering-team demo. Designed to run in a [zellij](https://zellij.dev) pane
next to a live demo pane.

## Render it

The deck is [presenterm](https://github.com/mfontanini/presenterm) Markdown
(Rust, single binary — fits the no-Python/no-Node project ethos):

```bash
brew install presenterm
presenterm docs/talk/deck.md
```

Navigation: arrow keys / `space` to advance, `r` to reload after edits,
`p` for the presenter view (speaker notes + next-slide preview), `q` to quit.

> Simpler alternative: `brew install slides` then `slides docs/talk/deck.md`
> (maaslalani/slides, Go). Fewer features; no speaker notes.

## Suggested zellij layout

Split into two panes — deck on the left, demo shell on the right:

- Left: `presenterm docs/talk/deck.md`
- Right: an MCP client (Claude Code / opencode) pointed at the live kengram
  server, ready to run the demo queries below.

## Demo queries (slide 6)

Run these from your MCP client against the live corpus:

- `search_thoughts("lessons from building kengram")` — returns dated
  `decision_record` thoughts about kEngram's own development.
- `get_thought(<id>)` — show the extracted `tags` (people / entities / topics /
  action_items / kind) on a hit.
- `get_related_thoughts(<id>)` — walk the relational graph (refines / replaces /
  supports / references edges).

Good scopes to browse: `project.kengram`, `engram.m3.dogfood`, `rjf.tech`.

**Rehearse 1–2 queries** so retrieval latency doesn't stall the finale, and keep
a backup screenshot in case the embedder is cold.
