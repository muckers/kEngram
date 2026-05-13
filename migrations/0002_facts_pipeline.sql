-- M2: facts pipeline.
--
-- Adds:
--   * pending_embeddings — durable queue table for async embedding. Capture
--     inserts a row; the worker drains it via SELECT ... FOR UPDATE SKIP LOCKED.
--   * facts_review_queue — landing zone for low-confidence facts awaiting
--     operator decision (accept | reject).
--   * reflector_runs — one row per reflector pass, backing facts.source_run_id
--     so a whole bad run can be jointly retracted later.
--   * facts.source_run_id — FK to reflector_runs (nullable: manual corrections
--     and pre-M2 rows leave it NULL).
--
-- See docs/milestones/m2-progress.md (operator decisions table) and
-- docs/milestones/m2-facts-pipeline.md (open-question answers) for the design
-- rationale behind each shape.

-- ---------------------------------------------------------------------------
-- pending_embeddings: durable FIFO queue for async embedding work.
-- ---------------------------------------------------------------------------
CREATE TABLE pending_embeddings (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_kind     TEXT NOT NULL CHECK (target_kind IN ('thought','artifact_chunk','fact')),
    target_id       UUID NOT NULL,
    model_id        TEXT NOT NULL,                    -- e.g. 'bge-m3:1024'
    enqueued_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempts        INT NOT NULL DEFAULT 0,
    last_attempt_at TIMESTAMPTZ,
    last_error      TEXT,
    UNIQUE (target_kind, target_id, model_id)
);

-- FIFO drain order. Worker pops oldest first via SKIP LOCKED.
CREATE INDEX pending_embeddings_dequeue_idx
    ON pending_embeddings (enqueued_at ASC);

-- ---------------------------------------------------------------------------
-- reflector_runs: one row per reflector pass. Backs facts.source_run_id.
-- ---------------------------------------------------------------------------
CREATE TABLE reflector_runs (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at             TIMESTAMPTZ,
    extractor_model         TEXT NOT NULL,
    extractor_version       INT NOT NULL,
    scope_filter            TEXT,                     -- NULL = all scopes
    n_thoughts_processed    INT NOT NULL DEFAULT 0,
    n_facts_committed       INT NOT NULL DEFAULT 0,
    n_review_queue          INT NOT NULL DEFAULT 0,
    error                   TEXT
);

CREATE INDEX reflector_runs_started_idx
    ON reflector_runs (started_at DESC);

-- ---------------------------------------------------------------------------
-- facts.source_run_id: FK to the run that produced the fact.
--
-- Nullable because manual corrections via the M2 `correct_fact` MCP tool
-- (extractor_model = 'manual', extractor_version = 0) have no run, and any
-- pre-M2 rows pre-date the column.
-- ---------------------------------------------------------------------------
ALTER TABLE facts
    ADD COLUMN source_run_id UUID REFERENCES reflector_runs(id);

CREATE INDEX facts_source_run_idx
    ON facts (source_run_id)
    WHERE source_run_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- facts_review_queue: low-confidence extractions awaiting operator decision.
-- ---------------------------------------------------------------------------
CREATE TABLE facts_review_queue (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    statement           TEXT NOT NULL,
    subject             TEXT,
    predicate           TEXT,
    object              TEXT,
    confidence          REAL NOT NULL CHECK (confidence BETWEEN 0 AND 1),
    source_thought_id   UUID REFERENCES thoughts(id) ON DELETE CASCADE,
    source_chunk_id     UUID REFERENCES artifact_chunks(id) ON DELETE CASCADE,
    extractor_model     TEXT NOT NULL,
    extractor_version   INT NOT NULL,
    source_run_id       UUID REFERENCES reflector_runs(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reviewed_at         TIMESTAMPTZ,
    decision            TEXT NOT NULL DEFAULT 'pending'
                          CHECK (decision IN ('pending','accept','reject')),
    CHECK (source_thought_id IS NOT NULL OR source_chunk_id IS NOT NULL)
);

-- Pending-review backlog scan: oldest first.
CREATE INDEX facts_review_queue_pending_idx
    ON facts_review_queue (created_at ASC)
    WHERE decision = 'pending';
