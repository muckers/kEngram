-- Qwen3 ANN projection path.
--
-- Raw qwen3 embeddings are 4096-dim vector rows. pgvector cannot HNSW-index a
-- vector above 2000 dims, and halfvec indexes cap at 4000 dims. Keep raw
-- embeddings as source of truth and add an indexed 3072-dim halfvec projection
-- for serving-time recall.

SET lock_timeout = '5s';
SET statement_timeout = '30min';

CREATE TABLE IF NOT EXISTS embedding_ann_projections (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    source_embedding_id UUID        NOT NULL REFERENCES embeddings(id) ON DELETE CASCADE,
    target_kind         TEXT        NOT NULL CHECK (target_kind IN ('thought','artifact_chunk','fact')),
    target_id           UUID        NOT NULL,
    model_id            TEXT        NOT NULL,
    model_version       INT         NOT NULL,
    projection_id       TEXT        NOT NULL,
    dimensions          INT         NOT NULL CHECK (dimensions = 3072),
    embedding           halfvec(3072) NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (source_embedding_id, projection_id),
    UNIQUE (target_kind, target_id, model_id, model_version, projection_id)
);

CREATE TABLE IF NOT EXISTS embedding_ann_projection_coverage (
    projection_id       TEXT        PRIMARY KEY,
    model_id            TEXT        NOT NULL,
    model_version       INT         NOT NULL,
    embedding_count     BIGINT      NOT NULL,
    projection_count    BIGINT      NOT NULL,
    missing_count       BIGINT      NOT NULL,
    status              TEXT        NOT NULL CHECK (status IN ('ok','diverged')),
    last_reconciled_at  TIMESTAMPTZ,
    last_checked_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS embedding_ann_projections_model_target_idx
    ON embedding_ann_projections (projection_id, target_kind, target_id);

WITH inserted AS (
    INSERT INTO embedding_ann_projections (
        source_embedding_id,
        target_kind,
        target_id,
        model_id,
        model_version,
        projection_id,
        dimensions,
        embedding
    )
    SELECT
        e.id,
        e.target_kind,
        e.target_id,
        e.model_id,
        e.model_version,
        'qwen3-embedding:halfvec:3072',
        3072,
        (l2_normalize(subvector(e.vector, 1, 3072)::vector(3072)))::halfvec(3072)
    FROM embeddings e
    WHERE e.model_id = 'qwen3-embedding'
      AND vector_dims(e.vector) >= 3072
    ON CONFLICT DO NOTHING
    RETURNING 1
)
INSERT INTO migration_audit (migration, rows_touched, notes)
SELECT
    '0013_qwen3_ann_projection',
    COUNT(*),
    'Added embedding_ann_projections sidecar and backfilled qwen3-embedding 3072-dim halfvec projections. Raw 4096-dim embeddings remain source of truth.'
FROM inserted;

SET max_parallel_maintenance_workers = 0;
SET maintenance_work_mem = '32MB';

CREATE INDEX IF NOT EXISTS embedding_ann_projection_qwen3_embedding_halfvec_3072_hnsw
    ON embedding_ann_projections
    USING hnsw (embedding halfvec_cosine_ops)
    WITH (m = 16, ef_construction = 100)
    WHERE projection_id = 'qwen3-embedding:halfvec:3072'
      AND target_kind = 'thought';

DO $$
DECLARE
    v_embedding_count  BIGINT;
    v_projection_count BIGINT;
    v_missing_count    BIGINT;
BEGIN
    SELECT COUNT(*)
      INTO v_embedding_count
      FROM embeddings e
     WHERE e.model_id = 'qwen3-embedding'
       AND vector_dims(e.vector) >= 3072;

    SELECT COUNT(*)
      INTO v_projection_count
      FROM embedding_ann_projections p
     WHERE p.projection_id = 'qwen3-embedding:halfvec:3072'
       AND p.model_id = 'qwen3-embedding';

    SELECT COUNT(*)
      INTO v_missing_count
      FROM embeddings e
     WHERE e.model_id = 'qwen3-embedding'
       AND vector_dims(e.vector) >= 3072
       AND NOT EXISTS (
           SELECT 1
             FROM embedding_ann_projections p
            WHERE p.source_embedding_id = e.id
              AND p.projection_id = 'qwen3-embedding:halfvec:3072'
       );

    INSERT INTO embedding_ann_projection_coverage (
        projection_id,
        model_id,
        model_version,
        embedding_count,
        projection_count,
        missing_count,
        status,
        last_reconciled_at,
        last_checked_at
    )
    VALUES (
        'qwen3-embedding:halfvec:3072',
        'qwen3-embedding',
        1,
        v_embedding_count,
        v_projection_count,
        v_missing_count,
        CASE
            WHEN v_missing_count = 0 AND v_projection_count = v_embedding_count THEN 'ok'
            ELSE 'diverged'
        END,
        NOW(),
        NOW()
    )
    ON CONFLICT (projection_id) DO UPDATE SET
        model_id = EXCLUDED.model_id,
        model_version = EXCLUDED.model_version,
        embedding_count = EXCLUDED.embedding_count,
        projection_count = EXCLUDED.projection_count,
        missing_count = EXCLUDED.missing_count,
        status = EXCLUDED.status,
        last_reconciled_at = EXCLUDED.last_reconciled_at,
        last_checked_at = EXCLUDED.last_checked_at;

    IF v_missing_count <> 0 OR v_projection_count <> v_embedding_count THEN
        RAISE EXCEPTION
            'ANN projection coverage mismatch after 0013: embeddings=%, projections=%, missing=%',
            v_embedding_count, v_projection_count, v_missing_count;
    END IF;
END $$;

ANALYZE embedding_ann_projections;
ANALYZE embedding_ann_projection_coverage;
