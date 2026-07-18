---
title: "kEngram"
sub_title: "A self-hosted, MCP-native memory for AI agents"
author: "Ron Forrester"
---

# kEngram

**A self-hosted, MCP-native memory for AI agents.**

One persistent backing store. Any MCP client — Claude Code, Claude Desktop,
opencode, ChatGPT — reads and writes the *same* memory.

- Rust + Postgres + `pgvector`. Self-hosted; no cloud dependency.
- Embedding + tagging are **external, swappable** sidecars.
- *ken* (to know) + *-gram* (a recorded mark) — a recorded unit of knowing.

Initial requirements:

- Fully local inference with option to connect to foundation models
- Fully local datastore (no cloud storage)
- Thoughts are immutable and deduped via sha256, can only be retracted never deleted
- Embeddings and tagging can be redone as models improve
- Relationships captured (a graph)


> The pipeline in one line:
> **capture → embed → tag → hybrid search**

<!-- speaker_note: |
  Don't linger here. The point is just: single brain, model-agnostic, self-hosted.
  The interesting parts are the architecture and the learnings. ~30 seconds.
-->

<!-- end_slide -->

# Architecture — the pipeline

<!-- column_layout: [3, 2] -->

<!-- column: 0 -->

```
      capture
         │
         ▼
     ┌───────┐
     │ queue │
     └───┬───┘
     ┌───┴───┐
     ▼       ▼
   embed    tag
     │       │
     ▼       ▼
  vectors   tags
```

- `capture` returns **instantly** — decoration runs in the background.
- One **worker** drains the queue, embedding + tagging each thought.
- `embed` and `tag` are **external sidecars** (a vector model + an LLM) —
  kEngram just calls their HTTP APIs (current served from docker container).

<!-- column: 1 -->

**Search — two ways of finding, blended**

- **By meaning** — semantic similarity
- **By wording** — exact terms, names, acronyms
- **Merge** the two into one ranked list
- **Re-rank** the top hits for relevance
- **Recency** — newer memories get a boost in the blend

<!-- reset_layout -->

<!-- speaker_note: |
  Async is the key shape: capture never blocks on the embedder or tagger.

  Under the hood (for Q&A): vector kNN over bge-m3 (1024-d) ∪ pg_trgm lexical,
  fused with reciprocal rank fusion (RRF), then a cross-encoder rerank and a
  recency half-life boost. Degrades gracefully — trigram still answers if the
  embedder is down (vector_search_available: false).
-->

<!-- end_slide -->

# Architecture — decisions that shaped it

<!-- new_line -->
<!-- list_item_newlines: 2 -->

- **Immutability spine.** Raw thoughts are permanent; tags + embeddings are
  *recomputable*. → A model swap is a **re-index, not a migration.**

- **Scopes** are hierarchical and organize content for association and retrieval

- **Content is identity.** SHA-256 fingerprint → idempotent capture; the same
  content twice collapses to one `thought_id`.

- **MCP-native.** The transport *is* the API. Every client shares one store —
  no per-client memory silos.

- **Closed-vocabulary relations.** 7 edge types (`replaces`, `requires`,
  `references`, `supports`, `belongs_to`, `decided_by`, `refines`) instead of
  open triples — predictable, with no extraction prompt to break under load.

<!-- speaker_note: |
  The immutability spine is the load-bearing idea. Because raw data is permanent
  and derived signals are disposable, you can re-tag or re-embed the whole corpus
  whenever a better model lands — no data migration, fully re-runnable.
-->

<!-- end_slide -->

# Learning #1 — a model ceiling that was really a deployment bug

<!-- new_line -->

Our best tagger is a **reasoning model**. At first it looked unusable — broken
JSON, repetition loops, timeouts. None of it was the model.

<!-- pause -->
<!-- new_lines: 1 -->
<!-- list_item_newlines: 1 -->

- The "model defect" was **context starvation** — it never had room to finish
  thinking, so it emitted truncated, broken JSON.
- **Never run a reasoning tagger at temperature 0** — greedy decoding makes its
  loops *deterministic* (~1/3rd of calls fail).
- The fix was **deployment, not model**: quantize to fit one GPU, pin an explicit
  context window, run at temp 0.2.

<!-- new_line -->

→ Tamed, it became the **best tagger we have.** Capability is in the model;
**unlocking it is in the deployment.**

<!-- speaker_note: |
  Timeline: qwen3-coder (Jun 6) → qwen3.6:35b (Jun 12) → gemma4:31b-qat-ctx16k
  (Jun 15, current). The flips weren't about smarter models. gemma "failed" the
  early rounds via context starvation — the OpenAI /v1 endpoint has no num_ctx,
  so Ollama silently loaded a tiny default and truncated the 4k–13k-token
  reasoning trace — plus temp-0 loops (~36% failures vs near-zero at 0.2). Fixes:
  bake an explicit context window into the Ollama model, QAT-quantize so it's
  GPU-resident on one 24GB card, run at temp 0.2. Then gemma won kind-accuracy
  0.829 vs qwen 0.70 on the golden corpus.
-->

<!-- end_slide -->

# Learning #2 — raw data permanent, derived signals disposable

<!-- new_line -->

The spine of the design: **a thought is immutable.** Its embedding and tags are
**recomputed**, never patched.

<!-- pause -->
<!-- new_lines: 1 -->
<!-- list_item_newlines: 1 -->

- Swap the embedding or tagging model? It's a **re-index, not a migration** —
  just re-run the pipeline over the corpus.
- That's *why* the tagger model could change **three times in a month** at zero
  data risk.
- Every derived step is **idempotent** — re-runnable by design; a bad tag can
  never corrupt the source.

<!-- new_line -->

> The captured text is ground truth. Everything else is a **cache you can rebuild.**

<!-- speaker_note: |
  This pays off Learning #1: each tagger switch was just a `kengram tag --force`
  pass over the corpus — no schema migration. When the 16k-context gemma landed
  with identical weights, no re-tag was even needed. DESIGN.md §2: "raw data is
  permanent, derived signals are recomputable." Immutability also means thoughts
  are never edited in place — retraction is a soft flag, not a delete.
-->

<!-- end_slide -->

# Learning #3 — you can't eyeball tag quality

<!-- new_line -->

Tag quality is **invisible at a glance** — a model can read fine and quietly
mislabel half the corpus.

<!-- pause -->
<!-- new_lines: 1 -->
<!-- list_item_newlines: 1 -->

- We first chose models by **gut and a handful of fixtures** — and kept choosing
  wrong.
- So we built a **golden-corpus eval harness**: hand-labeled thoughts, every
  candidate scored on the **finalized output** production actually stores.
- It runs **database-free** — eval never touches the live corpus — and reports
  accuracy, reliability, and stability, so a model wins on *evidence, not vibes.*

<!-- new_line -->

> The harness **overturned our gut twice** — and crowned the tagger we run today.

<!-- speaker_note: |
  The golden corpus is 116 hand-reviewed thoughts. Scoring is on *finalized*
  output (what production persists), with repeats for stability and per-field
  accuracy. This is exactly the harness that drove Learning #1's model flips
  (qwen3-coder → qwen3.6 → gemma-qat). The other half of "tagging is hard" — for
  Q&A — is prompt-level bugs (e.g. verb-as-name) caught by a deterministic
  model-independent "finalize floor," since small models pattern-match literally
  and prompt-whittling alone never fixes structural failures.
-->

<!-- end_slide -->

# The MCP surface — capture & retrieve

<!-- new_line -->

Every client speaks the **same small tool set**. Six for the read/write core:

| Tool | What it does |
| --- | --- |
| `capture` | Store a thought. Returns instantly; embed + tag run async. Idempotent on content (SHA-256). |
| `search_thoughts` | Hybrid search: vector ∪ trigram → RRF → recency → rerank. Filter by scope / tags. |
| `recent_thoughts` | Chronological browse of a scope — newest first, no scoring. |
| `get_thought` | Fetch one by ID with provenance: embedding status, tags, retraction. |
| `list_scopes` | Enumerate scopes in use, with thought counts and activity dates. |
| `retract_thought` | Mark a thought untrusted. A soft flag — never a delete. |

<!-- speaker_note: |
  These are the everyday calls. capture + search_thoughts carry 90% of traffic.
  retract is the only "mutation" — and it's a flag, not an edit (thoughts are immutable).
-->

<!-- end_slide -->

# The MCP surface — the relational graph

<!-- new_line -->

Three more tools turn the store into a **graph**. Edges are a closed vocabulary —
`replaces`, `requires`, `references`, `supports`, `belongs_to`, `decided_by`, `refines`:

| Tool | What it does |
| --- | --- |
| `link_thoughts` | Assert one edge from a thought to a thought / entity / person / URL. Idempotent. |
| `unlink_thoughts` | Soft-delete an edge by its `(from, relation, target)` triple. |
| `get_related_thoughts` | Walk outbound + inbound edges from a thought. The demo's third call. |

<!-- new_line -->

> Relations are **agent-supplied**, not extracted — no prompt to break under load.

<!-- speaker_note: |
  The closed vocabulary is the design bet from the decisions slide, made concrete:
  7 relations, polymorphic targets (thought/entity/person/url), soft-delete.
  search_thoughts → get_related_thoughts is the "discover then traverse" pattern
  the demo walks.
-->

<!-- end_slide -->

# Demo — it remembers its own construction

The content for this presentation came *out of the system itself*.

In the pane to the right →

```text
search_thoughts("lessons from building kengram")
   → dated decision_records about its own development

get_thought(<hit>)           → its extracted tags + provenance
get_related_thoughts(<hit>)  → walk the relational graph
```

<!-- pause -->

The talk you just heard is *retrievable from the thing the
talk is about.*

<!-- speaker_note: |
  Drop into the live pane here. Rehearse 1-2 queries so latency doesn't bite.
  Have a backup screenshot in case the server/embedder is slow.
  Scopes worth showing: project.kengram, engram.m3.dogfood, rjf.tech.
-->
