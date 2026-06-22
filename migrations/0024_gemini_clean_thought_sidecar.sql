-- Typed Gemini clean-text sidecar.
--
-- Raw thoughts.content stays unchanged for display and audit. The embedding
-- vector here is generated from kengram-clean-v1 text, with provenance columns
-- tying the row back to the raw content fingerprint and clean-text hash.

CREATE EXTENSION IF NOT EXISTS vector;

SET lock_timeout = '5s';
SET statement_timeout = '30min';

CREATE TABLE IF NOT EXISTS thought_embeddings_gemini_clean_v1 (
    id                  UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    thought_id          UUID          NOT NULL REFERENCES thoughts(id) ON DELETE CASCADE,
    model_id            TEXT          NOT NULL CHECK (model_id = 'gemini-embedding-001'),
    model_version       INT           NOT NULL DEFAULT 1 CHECK (model_version = 1),
    dimensions          INT           NOT NULL DEFAULT 3072 CHECK (dimensions = 3072),
    clean_strategy      TEXT          NOT NULL CHECK (clean_strategy = 'kengram-clean-v1'),
    clean_reason        TEXT          NOT NULL,
    content_fingerprint BYTEA         NOT NULL,
    clean_sha256        TEXT          NOT NULL,
    embedding           halfvec(3072) NOT NULL,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    UNIQUE (thought_id, model_id, model_version, clean_strategy)
);

CREATE INDEX IF NOT EXISTS thought_embeddings_gemini_clean_v1_model_idx
    ON thought_embeddings_gemini_clean_v1 (
        model_id,
        model_version,
        clean_strategy,
        thought_id
    );
