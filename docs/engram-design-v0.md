# Engram — Local Agent Memory Service

**Status:** Draft v0.1 · for review
**Working name:** Engram (placeholder; trivial to rename)
**Author:** [you]
**Reviewers:** [TBD]
**Last updated:** 2026-05-13

---

## 1. Summary

Engram is a self-hosted, MCP-native memory service for AI agents. It runs alongside vLLM (or equivalent) on a personal headless inference server, reachable from the operator's devices over Tailscale wherever they happen to be. It provides a persistent, model-agnostic backing store that any MCP-capable client (Claude Code, Claude Desktop, opencode, ChatGPT, Cursor, Gemini CLI, custom Rust agents) can read from and write to.

It is OB1's architectural shape — Postgres + pgvector + a thin MCP gateway — implemented as a single Rust binary, with the local vLLM endpoint serving as the embedding and extraction backend, designed so that swapping the underlying embedding or extraction model is a routine operation rather than a migration.

The deployment target is single-user, single-active-session. Concurrent multi-user serving is explicitly not in scope.

The system is built incrementally across five milestones (§3.5). The remainder of this document describes the *terminal* state — all five milestones complete. Inline milestone callouts (e.g. `[M1]`, `[M2+]`) flag features that arrive at a specific milestone. §3.5 is the source of truth for what ships when, and supersedes anything elsewhere in the document that reads as if a feature is "v0."

## 2. Goals

- **Single source of memory** across every agent and model the operator uses.
- **Model-independence** at the storage layer: changing embedding or extraction model must not invalidate captured content.
- **Local-first**: defaults run with no cloud dependency. Cloud is a configurable opt-in per provider.
- **Provenance-preserving**: every derived fact links to the immutable raw thought that produced it. Extraction drift must be detectable and correctable.
- **Tiered exposure**: localhost / mesh / public, configurable, with auth that scales accordingly.
- **Operationally simple**: single Postgres, single Rust binary, runs under systemd.

## 3. Non-goals

- Not an agent runtime (cf. Letta). Engram stores and retrieves; agents live elsewhere.
- Not a temporal knowledge graph (cf. Graphiti). Facts are timestamped and supersedable, but we do not model validity windows as first-class entities.
- Not a vector database product. We use pgvector and we are happy.
- Not multi-tenant SaaS. Single operator, optional shared with trusted humans.
- No ML training. We use existing embedding / instruct models as black boxes.

## 3.5 Milestone roadmap

The system is built in five capability milestones, preceded by a small environment-setup milestone (M0). Each capability milestone is independently shippable: at the end of M1 the operator has a usable memory service; subsequent milestones add capability without invalidating prior ones.

**M0 — Development environment.** *The floor under the floor.*
- Postgres 16 running in Docker via `docker-compose.yml` at the repo root, using the `pgvector/pgvector:pg16` image (bundles `vector`, `pg_trgm`, `pgcrypto`).
- Ollama (already installed on the operator's box) serves as the dev-mode embedder via its OpenAI-compatible endpoint (`http://localhost:11434/v1/embeddings`, model `bge-m3`). Production retains the TEI sidecar.
- `DEVELOPMENT.md` runbook for first-time setup. No code is written; M0 only ensures M1's code has somewhere to run.

**M1 — Capture and search.** *The floor.*
- Schema ships in full (`thoughts`, `embeddings`, `facts`, `artifacts`, `artifact_chunks`) but only `thoughts` and `embeddings` are populated. Future-milestone tables exist now so later migrations don't touch live data.
- Sync embedding on `capture` via TEI sidecar (BGE-M3, 1024-dim).
- Hybrid retrieval: vector kNN ∪ trigram lexical search, fused via reciprocal rank fusion (RRF). No reranker.
- Four MCP tools: `capture`, `search_thoughts`, `recent_thoughts`, `get_thought`.
- Single binary; subcommands `serve` and `migrate`. No worker process.
- Tier 0 auth (localhost-only). Tier 1 (Tailnet) is a config change, not a code change.

**M2 — Facts pipeline.**
- `engram-extract` crate becomes real with a vLLM client; `Extractor` trait gains its first implementation.
- Worker process appears (`engram worker` subcommand). Reflector cron job runs.
- `facts` table populated; new MCP tools `search_facts`, `correct_fact`.
- The async-embedding seam designed at M1 is exercised: `capture` posts a job; the worker computes the embedding.

**M3 — Search quality.**
- BGE-reranker (also via TEI) plugged in after RRF fusion. Retrieve top-50, rerank to top-N.
- MCP search interface unchanged; quality goes up.

**M4 — Artifacts.**
- Long-form ingestion: `artifacts` and `artifact_chunks` populated. Chunking strategy lands here.
- New MCP tool: `ingest_artifact`.
- Search results unify thoughts and chunks under one ranking.

**M5 — Operational maturity.**
- Prometheus `/metrics` endpoint.
- Tier 2 bearer-token auth + audit log.
- Backup tooling (scripts, retention policy).
- Eval suite (capture-recall, cross-model retrieval consistency, LongMemEval-style).
- The `stats` MCP tool.

**Order rationale.** M1 is the floor: nothing else makes sense without capture and retrieval. M2 (facts) before M3 (rerank) because facts add capability and rerank improves quality, and quality without capability is unmotivated. M4 (artifacts) before M5 (operational) because ingesting existing notes/transcripts earns its keep faster than auth/eval ceremony for a single-operator tool.

## 4. High-level architecture

```
                   ┌──────────────────────────────────────────┐
                   │             Engram (single binary)       │
                   │                                          │
  MCP clients      │   ┌──────────┐    ┌────────────────┐     │
  (Claude Code, ──→│──→│ MCP/HTTP │───→│   Core service │     │──┐
   Desktop, etc.)  │   │  surface │    │  (capture,     │     │  │
   over Tailscale  │   └──────────┘    │   retrieval,   │     │  │
                   │                   │   reflection)  │     │  │
                   │   ┌──────────┐    └────────────────┘     │  │
                   │   │  Worker  │            │              │  │
                   │   │  (cron)  │────────────┘              │  │
                   │   └──────────┘   [M2+]                   │  │
                   │         │                                │  │
                   │         ▼                                │  │
                   │   ┌──────────────────────────────────┐   │  │
                   │   │  Embedder + Extractor (traits)   │   │  │
                   │   │  default: OpenAI-compatible      │   │  │
                   │   └──────────────────────────────────┘   │  │
                   │                  │                       │  │
                   └──────────────────┼───────────────────────┘  │
                                      ▼                          ▼
                            ┌──────────────────┐         ┌────────────┐
                            │  vLLM endpoint   │         │ Postgres   │
                            │  (instruct +     │         │ + pgvector │
                            │   embedding,     │         │            │
                            │   localhost:8000)│         └────────────┘
                            └──────────────────┘
                                      │
                                      ▼
                            ┌──────────────────┐
                            │  RTX 3090(s)     │
                            └──────────────────┘
```

Engram is a *client* of the local vLLM endpoint, not the operator of it. vLLM is presumed to be serving primary inference traffic to other Tailscale-connected devices anyway; Engram piggybacks on that infrastructure. Three logical components, one binary:

- **MCP/HTTP surface.** Streamable HTTP transport speaking MCP. Same binary also exposes an admin HTTP API.
- **Core service.** Capture, search, fact retrieval, scope management.
- **Worker.** [M2+] Periodic session reflection, deferred re-embedding, fact compaction. Runs in-process with a Tokio scheduler when the binary is launched in `worker` mode. **The worker process does not exist in M1**; capture-side embedding is synchronous in the server process.

## 5. Data model

The model is deliberately small. Three primary entities — thoughts, embeddings, facts — plus an artifacts table for long-form content. Embeddings are intentionally a separate first-class table so model swaps are routine rather than migrations.

```sql
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Raw, immutable captures. Single source of truth.
CREATE TABLE thoughts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope           TEXT NOT NULL DEFAULT 'global',
    content         TEXT NOT NULL,
    source          TEXT NOT NULL,           -- 'manual', 'agent:claude-code', 'reflector', etc.
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata        JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX thoughts_scope_recent_idx
    ON thoughts (scope, created_at DESC);
CREATE INDEX thoughts_content_trgm_idx
    ON thoughts USING gin (content gin_trgm_ops);

-- Long-form content. Reserved for M4.
CREATE TABLE artifacts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope           TEXT NOT NULL DEFAULT 'global',
    kind            TEXT NOT NULL,           -- 'document'|'transcript'|'code'|'web'|...
    title           TEXT,
    content_uri     TEXT,                    -- file:// or s3:// for blobs
    content_text    TEXT,                    -- inline if small
    metadata        JSONB NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE artifact_chunks (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    artifact_id     UUID NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    chunk_index     INT NOT NULL,
    content         TEXT NOT NULL,
    UNIQUE (artifact_id, chunk_index)
);

-- Embeddings are first-class. Multiple per target during model migration.
CREATE TABLE embeddings (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_kind     TEXT NOT NULL CHECK (target_kind IN ('thought','artifact_chunk','fact')),
    target_id       UUID NOT NULL,
    model_id        TEXT NOT NULL,           -- e.g. 'bge-m3:1024'
    model_version   INT NOT NULL DEFAULT 1,
    vector          vector(1024) NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (target_kind, target_id, model_id, model_version)
);

-- One HNSW partial index per active embedding model. M1 ships this one.
-- Adding a new model = a future migration adds a new partial index over
-- the same table; old rows stay; the active-model concept lives in config
-- (see §9), not in a Postgres GUC.
CREATE INDEX embeddings_bge_m3_hnsw
    ON embeddings USING hnsw (vector vector_cosine_ops)
    WHERE model_id = 'bge-m3:1024';

-- Reserved for M2.
CREATE TABLE facts (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope               TEXT NOT NULL,
    statement           TEXT NOT NULL,       -- natural-language fact
    subject             TEXT,                -- optional structured triple
    predicate           TEXT,
    object              TEXT,
    source_thought_id   UUID REFERENCES thoughts(id) ON DELETE CASCADE,
    source_chunk_id     UUID REFERENCES artifact_chunks(id) ON DELETE CASCADE,
    extractor_model     TEXT NOT NULL,
    extractor_version   INT NOT NULL,
    confidence          REAL NOT NULL CHECK (confidence BETWEEN 0 AND 1),
    superseded_by       UUID REFERENCES facts(id),
    superseded_at       TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (source_thought_id IS NOT NULL OR source_chunk_id IS NOT NULL)
);

CREATE INDEX facts_active_idx
    ON facts (scope, created_at DESC)
    WHERE superseded_at IS NULL;
```

**Why embeddings are a separate table.** A model swap is a re-index, not a re-write. With this layout we insert a new row in `embeddings` per `(target, new model)`, build a new HNSW partial index for the new model, and once the operator is satisfied with retrieval quality, optionally drop the old rows and old index. No data is lost during the swap.

**One HNSW index per model.** Each active embedding model gets its own partial index, predicated on a literal `model_id` string. This is required for correctness: Postgres demands partial-index predicates be `IMMUTABLE`, and `current_setting()` is `STABLE`. The "active embedder" is therefore a config concern (see §9), not a database GUC. Operationally, swapping the active model means: ship a migration that adds the new partial index, update config to point at the new `model_id`, restart.

**Scoping.** Free-form string, default `global`. Convention rather than enforcement: `work.tcgplayer`, `personal`, `project.engram`, etc. A `scopes` registry table can come later if introspection is wanted.

## 6. Ingest path

There are two write paths. Both terminate in the same `thoughts` row plus an embedding.

1. **Direct capture.** [M1] Agent calls `capture(content, scope?, source?, metadata?)`. The handler inserts the thought, computes its embedding via TEI, writes the embedding row, returns the thought ID. **In M1 this is fully synchronous** — capture returns when the embedding is durable. At low-hundreds-of-captures-per-day with TEI sidecar latency under 200 ms, the wait is invisible.

2. **Artifact ingestion.** [M4] Agent calls `ingest_artifact(uri, kind, scope?)`. The handler inserts the artifact row and hands off to the worker, which fetches, chunks, embeds, and writes `artifact_chunks` plus their embeddings.

**Designed-in seam for async embedding.** [M2+] In M1 the capture handler calls `Embedder::embed(...)` directly. In M2 the worker process appears, and the same capture handler is changed in *one place*: instead of calling `embed` inline, it enqueues a job; the worker drains the queue. The MCP tool contract stays identical; capture continues to return a thought ID immediately; the embedding row becomes available shortly after (with a brief window during which `search_thoughts` may not surface the brand-new thought via vector — trigram still finds it).

The worker also runs the **session reflector** [M2+]. Periodically (configurable; default `0 3 * * *` daily) it walks recent thoughts in a scope, asks the extractor to derive structured facts, and writes them with full provenance. See §10 for drift handling.

## 7. Retrieval path

Three retrieval primitives, composable:

- **Semantic** — vector kNN over the active embedding model.
- **Lexical** — `pg_trgm` similarity over `content`. Cheap; complements vector search for proper nouns, acronyms, and code identifiers — exactly the queries pure embeddings are notoriously bad at.
- **Recency** — `ORDER BY created_at DESC` with a scope filter.

**Default `search_thoughts` from M1 is a hybrid.** Concretely:

1. Run two SQL queries in parallel: a vector kNN limited to top-K against the active model's HNSW index; a trigram similarity query limited to top-K against `thoughts_content_trgm_idx`.
2. Fuse the result sets with reciprocal rank fusion (RRF): `score(d) = Σᵢ 1 / (k + rankᵢ(d))` over the two rankings, with `k` typically 60.
3. Apply scope filtering and a recency boost (multiplicative `exp(-age/τ)` with `τ` = 30 days, configurable per call). Return the top N.

Why RRF over a weighted-score blend like `α·cos_sim + β·bm25 + γ·exp(-age/τ)`: RRF is parameter-light, robust to score-distribution differences between heterogeneous rankers, and is the de-facto choice for vector + lexical hybrids in current information-retrieval literature. It also generalizes cleanly to a third ranker when the M3 reranker is added.

```rust
pub struct SearchRequest {
    pub query: String,
    pub scope: Option<String>,
    pub limit: usize,                 // default 10
    pub mode: SearchMode,             // Hybrid | Semantic | Lexical | Recent
    pub recency_half_life_days: f32,  // default 30
    pub include_facts: bool,          // [M2+] attach extracted facts to results
}
```

**Reranker.** [M3] M3 adds a cross-encoder rerank pass after RRF fusion: retrieve a wider candidate set (typically top-50), rerank with BGE-reranker via TEI to get the final top-N. The MCP search interface is unchanged.

## 8. MCP surface

Tools and the milestone in which each ships. Names and signatures are part of the contract once shipped.

| Tool | Milestone | Purpose |
|---|---|---|
| `capture` | M1 | Store a thought. Returns `thought_id`. |
| `search_thoughts` | M1 | Hybrid retrieval over thoughts. |
| `recent_thoughts` | M1 | Browse by recency in a scope. |
| `get_thought` | M1 | Full thought + provenance. (Linked-facts join lights up at M2.) |
| `search_facts` | M2 | Retrieval over extracted facts. |
| `correct_fact` | M2 | Human/agent override: supersede or delete an extracted fact. |
| `ingest_artifact` | M4 | Async ingest of a longer document. |
| `stats` | M5 | Per-scope counts, last activity, embedding model version. |

`correct_fact` (M2) matters: it is the explicit user-facing knob for "the extractor got it wrong." It writes a new fact pointing at the same source thought and marks the old one superseded.

## 9. Embedding & extraction abstraction

Two traits, one config struct, no other architectural concession to model choice.

```rust
#[async_trait]                      // [M1]
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &'static str;
    fn dimensions(&self) -> usize;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

#[async_trait]                      // [M2]
pub trait Extractor: Send + Sync {
    fn model_id(&self) -> &'static str;
    fn version(&self) -> u32;
    async fn extract(
        &self,
        thought: &str,
        ctx: &ExtractionContext,
    ) -> Result<Vec<ExtractedFact>>;
}

pub struct ExtractedFact {          // [M2]
    pub statement: String,
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub confidence: f32,
}
```

**Default implementations:**

- **`TeiEmbedder`** [M1] — calls a `text-embeddings-inference` (TEI) sidecar over HTTP. Default `endpoint = http://localhost:8080`. Default model `BAAI/bge-m3`, `model_id = "bge-m3:1024"`, 1024 dimensions.
- **`CloudEmbedder`** [M1] — calls Voyage AI or OpenAI embeddings. Intended for development environments where a local TEI sidecar isn't running. Off by default.
- **`OpenAICompatibleExtractor`** [M2] — calls vLLM `/v1/chat/completions` with structured-output prompting (JSON Schema or grammar-constrained). Default `endpoint = http://localhost:8000/v1`. The model called is whatever vLLM is currently serving — we don't pin or pick it.
- **`OpenRouterExtractor`** [M2] — opt-in cloud fallback for testing the model-swap path or when the local box is offline.

The trait boundary is the buffer-from-model-changes guarantee. Swapping vLLM's served model, swapping to SGLang, swapping to a cloud provider — all happen behind the same interface. The only operation that propagates beyond the trait is a re-embed when the *embedder* changes (which is why §5 makes embeddings a separate table).

**Active-embedder selection.** From M1 onward the active embedder is identified by `model_id` (e.g. `bge-m3:1024`) and is a config field — the engram TOML declares which model is active; that string must match the predicate of an existing HNSW partial index. There is intentionally no Postgres-side GUC.

Configuration is a TOML file:

```toml
[database]
# Postgres connection. Overridden by DATABASE_URL env var if set (sqlx convention).
url = "postgres:///engram"
max_connections = 10

[embedder]                                      # [M1]
provider     = "tei"
endpoint     = "http://localhost:8080"
model        = "BAAI/bge-m3"
model_id     = "bge-m3:1024"                    # must match an HNSW partial index
dimensions   = 1024

[extractor]                                     # [M2+]
provider              = "openai-compatible"     # vLLM
endpoint              = "http://localhost:8000/v1"
model                 = "Qwen/Qwen2.5-Coder-32B-Instruct-AWQ"
temperature           = 0.1
max_facts_per_thought = 8
response_format       = "json_schema"

[reflector]                                     # [M2+]
enabled                  = true
schedule                 = "0 3 * * *"          # cron: 3am daily, single-user friendly
batch_size               = 50
min_confidence_to_store  = 0.6
review_queue_below       = 0.4
```

**Hardware sizing — concrete on the Phase 1 / Phase 2 BOM, single-user.**

The box is a personal inference server: one operator, one active session at a time, accessed over Tailscale from wherever the operator is. There is no concurrent multi-user load to budget for. The binding constraint is fitting the served instruct model + embedder + a single session's KV cache in available VRAM.

**Phase 1 (single RTX 3090, 24 GB VRAM):**

The default optimizes for tool-use quality, since the operator's stated use case is opencode / Claude Code against the local endpoint:

| Component | Choice | VRAM |
|---|---|---|
| vLLM-served instruct | Qwen2.5-Coder-32B-Instruct AWQ-int4 | ~19 GB |
| Embedder | BGE-M3 in TEI, **CPU build** | 0 GB (system RAM) |
| **KV cache headroom** | | **~5 GB → ~32K tokens single-session** |

CPU embeddings via TEI on the 9800X3D run at ~50–150 ms per call. Engram's actual call rate is a few embeddings per minute at peak personal use, not thousands, so the latency is invisible. The trade is real: capture latency goes from ~10 ms (GPU TEI) to ~100 ms (CPU TEI), and ~5 GB of KV cache headroom comes back to vLLM. For single-user code-agent work that almost always stays under 32K tokens, this is the right deal.

**Why Coder-32B over a smaller model.** For strong tool use against opencode / Claude Code, model quality at the tool-call schema and multi-step planning level matters more than peak throughput. Qwen2.5-Coder-32B is one of the few open models where tool calling holds up under real agent loops — error recovery, multi-step planning, long tool-result reasoning. A 14B class model is sufficient for Engram's *own* extraction needs but underperforms on the operator's primary use case.

**Reflection cost** [M2+]. A reflector pass over 50 thoughts is ~4k input tokens → ~1k structured output. At Coder-32B's vLLM throughput on a 3090 (≈75 tok/s per stream per the BOM), that's roughly 60 seconds. Default schedule is `0 3 * * *` (3am daily); contention with active agent work is non-existent at that hour. If the operator needs more frequent or real-time extraction for a particular scope, the schedule is per-scope tunable.

**Embedder placement is a deployment-time choice, not a code change.** TEI ships CPU and CUDA builds with identical HTTP APIs (`ghcr.io/huggingface/text-embeddings-inference:cpu-1.x` vs `:1.x`). Switching is a systemd unit edit; the Engram TOML doesn't change. CPU is the v0 default; GPU is appropriate later if capture rate grows or the operator wants sub-100ms capture latency for some interaction pattern.

**Phase 2 (dual RTX 3090, 48 GB VRAM):**

Phase 2 is a quality upgrade rather than a necessity-driven one — Phase 1 single-user is genuinely a credible primary daily driver. The upgrade unlocks:

- Qwen2.5-Coder-32B at Q6/Q8 (better quality than AWQ-int4) with full KV cache via tensor-parallel
- 70B-class general models at Q4 (Llama 3.3 70B, Qwen 2.5 72B) for harder reasoning tasks, ~32 tok/s per the BOM
- DeepSeek-V2.5/V3 (235B MoE, ~21B active) at Q4 — explicitly strong at agentic work, ~25 tok/s per the BOM

vLLM's `--tensor-parallel-size 2` is the obvious deployment shape. The embedder either stays on CPU or moves to a single card via TEI's CUDA build; both are easy.

**System RAM and storage.** Postgres + pgvector will be MB-to-low-GB scale even with 100k+ thoughts; the 64 GB system RAM is overprovisioned for Engram's purposes (and is there for vLLM's CPU offload / weights loading anyway). With CPU embedding the embedder also runs out of system RAM — BGE-M3 is ~2 GB resident — well within budget. On the 2 TB NVMe, Engram's footprint is dominated by the database (single-digit GB at realistic scale); vLLM model weights are the actual storage hog.

## 10. Provenance, extraction drift, and reconciliation

This entire section describes M2+ behavior — M1 has no extractor and therefore no facts to drift. The mechanisms below land alongside the facts pipeline at M2.

This section addresses the operator's specific concern: *"I don't want there to be drift from truth when the session reflector extracts facts."*

**Five mechanisms, in order of importance:**

1. **Raw thoughts are immutable.** Extractors never modify `thoughts`. They only write to `facts` with a foreign key back. If extraction is wrong, the truth is still recoverable.

2. **Every fact carries its extractor identity.** `extractor_model` + `extractor_version` on every row. When the local extractor is upgraded, `WHERE extractor_version < N` finds every fact that needs re-evaluation.

3. **Confidence-gated commit.** The extractor returns a self-rated confidence. Below `review_queue_below` (default 0.4), facts go to a review queue, not to `facts`. Between that and `min_confidence_to_store` (default 0.6), they're stored but flagged. Above, they're stored normally. Tunable per scope.

4. **Dual-extractor reconciliation (optional).** When `extractor.dual_run = true`, every reflection pass runs two distinct models (e.g. local Qwen3 and cloud Claude Haiku) and only commits facts both produced. Disagreements surface as review-queue items. Roughly doubles cost; recommended for high-stakes scopes only.

5. **Re-extraction is a first-class operation.** `engram reflect --rerun --scope work --since 2026-01-01` re-runs the current extractor against historical thoughts and reconciles. New facts that match existing ones (same `(subject, predicate, object)`) are merged; conflicting ones supersede the old via `superseded_by`. The audit trail is preserved.

**What this does not protect against:** a confident-and-wrong extractor producing a high-confidence wrong fact that no other extractor disagrees with. The mitigation is human review via `correct_fact` and periodic `engram audit` reports that surface low-traffic facts (potentially stale) and high-confidence facts that contradict source thoughts on lexical inspection.

## 11. Deployment & ops

**Target hardware:** Phase 1 of the BOM — RTX 3090 (24 GB), Ryzen 7 9800X3D, 64 GB DDR5-6000, 2 TB PCIe 5.0 NVMe, Ubuntu 24.04 LTS, NVIDIA driver 560+, CUDA 12.6+. Postgres 16+ with `pgvector` ≥ 0.7 (HNSW required), `pg_trgm`, `pgcrypto`. Phase 2 (dual 3090) is fully supported by the same software stack with one config change (`CUDA_VISIBLE_DEVICES`).

**Components:**

- `engram` — the single Rust binary. M1 supports `serve` and `migrate` subcommands; `worker` joins at M2.
- Postgres 16 with `pgvector` ≥ 0.7, `pg_trgm`, `pgcrypto`. Connection is configured by URL (TOML or `DATABASE_URL` env). Local Unix socket is the simplest deployment; remote TCP — same Tailnet, separate NAS or DB host, or anywhere reachable — is fully supported. **Extensions must be installed on the Postgres server**, not the Engram host. At personal-scale data with HNSW indexes, network round-trip on a LAN adds negligible latency to queries.
- `text-embeddings-inference` HTTP server for BGE-M3, sidecar pattern. **CPU build by default** for v0; swap to CUDA build by changing the systemd unit's container image (no Engram code or config change needed). Required from M1.
- vLLM serving an instruct model — required from M2 onward (no extractor in M1). **Operated independently of Engram.** Engram is a client; the operator manages vLLM's lifecycle, model choice, and serving config. Engram only requires the OpenAI-compatible endpoint to be reachable.

**Process model:** systemd units. `engram-server.service` exists from M1. `engram-tei.service` (the embeddings sidecar; CPU build by default — see §9) is required from M1. `engram-worker.service` joins at M2. vLLM and Postgres run as their own units, managed independently. The reflector is a Tokio-scheduled task inside the worker process (M2+), firing on a cron schedule (default `0 3 * * *`).

**Why a cron schedule rather than continuous** [M2+]. Single-user means the reflector competes only with the operator's own active agent sessions for vLLM throughput. Scheduling it for off-hours (overnight, default) eliminates that contention entirely. If the operator wants more aggressive extraction for a specific scope, the schedule is per-scope tunable via the admin API.

**Backups:** `pg_dump --format=custom` nightly to a separate disk; weekly to a remote (Backblaze B2 or rsync.net). Embeddings are derived data and don't strictly need backing up — `engram reflect --rebuild-embeddings` regenerates them — but including them speeds disaster recovery.

**Migrations:** `sqlx migrate`. Schema changes ship with the binary.

**Observability** [M5]. Structured `tracing` logs to journald are present from M1. The Prometheus `/metrics` endpoint exposing capture-rate, search-latency P50/P95/P99, embedding-queue depth, extractor failures, and fact-review-queue size lands at M5.

## 12. Auth & network exposure

Three relevant tiers. They map to milestones, not to deployment options offered all at once.

| Tier | Network | Auth | Milestone | Use case |
|---|---|---|---|---|
| **0 — Localhost** | `127.0.0.1` only | None | M1 | First-run validation; the development default. |
| **1 — Mesh** | Tailscale / WireGuard | None (mesh = auth) | M1 (config change) | Personal devices already on the Tailnet. The ops-recommended endpoint for single-user deployment. |
| **2 — Tunnel** | Cloudflare Tunnel / Caddy + LE | Bearer token | M5 | Non-Tailnet clients (Claude Desktop, ChatGPT) that need a public HTTPS MCP URL. |

A "Tier 3 — public + multi-user" option exists in principle but is **explicitly out of scope** for the current roadmap. It would require OAuth2, per-client tokens, and audit log; implementable later if the system is genuinely shared with another person, which is not a current requirement.

**Tier 1 is the recommended endpoint for single-user deployment.** Engram binds to the Tailnet interface and is reachable as `engram.tailXXXX.ts.net` from every personal device, using the same MagicDNS pattern as vLLM. No code change vs. Tier 0; only the bind address.

**Auth at Tier 2** [M5]. Bearer token validated against a hashed allowlist in `engram_tokens`. Tokens carry a scope-list — a token can be locked to `work.*` and not see `personal.*`. Audit log records `(token_id, tool, args_hash, ts)` for every call.

## 13. Evaluation

[M5] — eval suite ships at the operational-maturity milestone. We don't ship without it because "did the model swap regress retrieval" is the kind of question we'll ask ourselves often.

**Three suites, all reproducible from a fixture corpus:**

1. **Capture-recall.** Synthetic conversations seeded with target facts; check that subsequent semantically-relevant queries surface the right thoughts and facts.
2. **Cross-model retrieval consistency.** Re-embed the same fixture with a new embedder; measure overlap of top-10 results vs. baseline. Drop > 30% triggers a manual review before the swap is committed in production scopes.
3. **LongMemEval-style.** Subset of the public benchmark adapted to our schema. Apples-to-apples comparison against published Mem0 / Zep / Letta numbers.

Eval runs end-to-end in `engram eval --suite <name>` and dumps a JSON report.

## 14. Open questions

Resolved during the milestone-roadmap planning conversation (see Revision history):

1. ~~**Inference box specs.**~~ Resolved: Phase 1 RTX 3090 / 9800X3D / 64 GB; Phase 2 adds a second 3090.
2. ~~**v0 scope.**~~ Resolved: see §3.5 milestone roadmap. M1 = capture + hybrid search + MCP; facts/extractor/worker deferred to M2.
3. ~~**Search architecture.**~~ Resolved: hybrid (vector ∪ trigram, RRF) at M1; reranker at M3.
4. ~~**Active-embedder mechanism.**~~ Resolved: config-driven `model_id`, one HNSW partial index per model.

Carrying forward:

5. **Naming.** Engram is a placeholder. (Hippocampus, Cortex, Lattice, Mneme are all in the drawer.)
6. **Sync.** Do we ever want multi-machine replication? Logical replication on Postgres is straightforward, but only worth doing if you'll actually use it. Defer.
7. **Capture UX.** OB1's Slack capture is clever. Equivalents: a Telegram bot, a CLI `engram capture`, a Raycast/Alfred extension, a browser extension. Out of scope until at least M5.
8. **Embedding model default.** v0 commits to BGE-M3 (well-established, multilingual, runs in ~1.5 GB, supports rerank). A future milestone should bake off Qwen3-Embedding-4B and Qwen3-Embedding-8B against our own eval fixture before any production-scope re-embed. The embeddings table design (§5) makes this a routine swap rather than a migration.
9. **Are we storing agent transcripts?** Currently artifacts can hold them (M4+); we haven't decided whether agents auto-capture session transcripts on close or whether that's an explicit flush.
10. **Extractor model: dense vs. MoE.** Phase 2 unlocks Qwen3-30B-A3B (MoE, 3B active) as an alternative to Qwen2.5-32B (dense). The MoE option likely wins on throughput; quality on our specific extraction prompts is unmeasured. Decide via the eval suite (M5).

## 15. Out of scope (for the foreseeable future)

- Knowledge-graph reasoning (Cognee/Graphiti territory).
- Memory forgetting / TTL policies (everything is forever; pruning is a post-M5 conversation).
- Multi-modal memory (images, audio).
- Federated query across multiple Engram instances.
- A web UI. Postgres + `psql` is the admin interface.
- Public + multi-user deployment ("Tier 3" in §12).

## Revision history

- **2026-05-09** — Initial v0 draft by Claude Desktop in a "technical PM" capacity.
- **2026-05-09** — Revised by engineer + architect after the milestone-roadmap brainstorm. Added §3.5 milestone roadmap. Corrected schema in §5: added `CREATE EXTENSION` lines for `pgcrypto`/`vector`/`pg_trgm`; removed trailing comma in `thoughts`; replaced the `current_setting`-based partial HNSW index (which the Postgres planner rejects, since `current_setting` is `STABLE` not `IMMUTABLE`) with a literal-model partial index (`embeddings_bge_m3_hnsw`); added `thoughts_scope_recent_idx` and `thoughts_content_trgm_idx`; added `target_kind` CHECK on `embeddings`. Reframed §6 (M1 sync embedding via TEI; M2+ async seam), §7 (RRF hybrid; reranker M3), §8 (per-tool milestone column), §9 (Embedder M1, Extractor M2; `CloudEmbedder` added; active-embedder via config). Reframed §12 auth tiers as a milestone progression and dropped Tier 3 from the table. Pruned resolved open questions in §14. Doc now describes the M5-complete terminal state with milestone callouts inline.
- **2026-05-13** — **M2 complete.** Shipped in four phases A–D (see `docs/milestones/m2-progress.md`). Facts pipeline live: async embedding seam (capture enqueues; `engram worker` drains), reflector cron via `tokio-cron-scheduler` 0.15 (default off — opt-in via `[reflector] enabled = true`), `OpenAICompatibleExtractor` covering vLLM and OpenRouter via named-constructor presets, two new MCP tools (`search_facts`, `correct_fact`), `get_thought` now carries active `linked_facts`, and a new `engram reflect` subcommand with `--rerun [--since <RFC3339>]` for re-extracting historical thoughts (idempotent; supersedes on (S,P,O)-match-but-statement-differs; additive only). **Phase D simplification:** `search_facts` ships trigram-only inside an RRF-shaped pipeline — fact embeddings are wired through migration 0001's `target_kind = 'fact'` enum but the worker doesn't yet enqueue facts; the vector leg lands in M3 (search quality) alongside the cross-encoder reranker. **`correct_fact` provenance:** manual rows use the sentinel `extractor_model = "manual"`, `extractor_version = 0`, `source_run_id = NULL`, `confidence = 1.0`. Three-band confidence routing (the "flagged but committed" middle band from §_section_) is deferred — needs a `flagged` column on `facts` that doesn't exist yet. M2 success criteria #1–#5 met by code; #6 (operator dogfood ≥ 1 week) is the only remaining open item.
