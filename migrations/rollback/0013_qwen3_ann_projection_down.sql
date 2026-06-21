-- Manual rollback for forward migration 0013_qwen3_ann_projection.sql.
--
-- Use only after stopping binaries that depend on ANN projection search.
-- Raw embeddings remain the source of truth; dropping this sidecar returns
-- search to the exact raw-vector path until 0013 is re-applied.

SET lock_timeout = '5s';
SET statement_timeout = '5min';

DROP INDEX IF EXISTS embedding_ann_projection_qwen3_embedding_halfvec_3072_hnsw;
DROP INDEX IF EXISTS embedding_ann_projection_qwen3_3072_hnsw;
DROP INDEX IF EXISTS embedding_ann_projections_model_target_idx;
DROP TABLE IF EXISTS embedding_ann_projection_coverage;
DROP TABLE IF EXISTS embedding_ann_projections;

DELETE FROM migration_audit
WHERE migration = '0013_qwen3_ann_projection';
