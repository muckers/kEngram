-- no-transaction

-- Direct ANN index for the Gemini clean-text typed sidecar.
-- The sidecar stores `halfvec(3072)`, so serving does not need the generic
-- qwen projection table and cannot accidentally search raw boilerplate rows.

CREATE INDEX CONCURRENTLY IF NOT EXISTS thought_embeddings_gemini_clean_v1_hnsw
    ON thought_embeddings_gemini_clean_v1
    USING hnsw (embedding halfvec_cosine_ops)
    WITH (m = 16, ef_construction = 100)
    WHERE model_id = 'gemini-embedding-001'
      AND model_version = 1
      AND clean_strategy = 'kengram-clean-v1';
