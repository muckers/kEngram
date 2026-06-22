-- Typed bge-m3 sidecar.
--
-- Do not widen embeddings.vector: qwen3 stays on vector(4096) plus the
-- halfvec projection path. bge-m3:1024 stores directly in this sidecar.

CREATE EXTENSION IF NOT EXISTS vector;

SET lock_timeout = '5s';
SET statement_timeout = '30min';

CREATE TABLE IF NOT EXISTS thought_embeddings_bge_m3 (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    thought_id      UUID        NOT NULL REFERENCES thoughts(id) ON DELETE CASCADE,
    model_id        TEXT        NOT NULL CHECK (model_id = 'bge-m3:1024'),
    model_version   INT         NOT NULL DEFAULT 1 CHECK (model_version = 1),
    dimensions      INT         NOT NULL DEFAULT 1024 CHECK (dimensions = 1024),
    embedding       vector(1024) NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (thought_id, model_id, model_version)
);

CREATE INDEX IF NOT EXISTS thought_embeddings_bge_m3_model_idx
    ON thought_embeddings_bge_m3 (model_id, model_version, thought_id);

