-- no-transaction

-- Direct ANN index for the bge-m3:1024 typed sidecar. This stays structural:
-- the indexed column is vector(1024), so serving does not depend on matching a
-- textual expression cast.

CREATE INDEX CONCURRENTLY IF NOT EXISTS thought_embeddings_bge_m3_hnsw
    ON thought_embeddings_bge_m3
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 100)
    WHERE model_id = 'bge-m3:1024'
      AND model_version = 1;

