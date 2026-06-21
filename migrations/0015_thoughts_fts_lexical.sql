-- no-transaction

-- Fast lexical retrieval for hybrid search.
--
-- The old pg_trgm similarity leg was too non-selective on large legacy blobs
-- and forced multi-second scans. Postgres FTS gives the lexical leg an
-- inverted index while keeping raw thought content as the source of truth.
--
-- Safety: this is a live-table index on `thoughts`, so it must remain
-- CONCURRENTLY and must be the only statement in this no-transaction
-- migration file. Migration 0014 sets max_parallel_maintenance_workers=0 in
-- the same SQLx migrator session before this build starts.

CREATE INDEX CONCURRENTLY IF NOT EXISTS thoughts_content_fts_idx
    ON thoughts
    USING gin (to_tsvector('english', content))
    WHERE retracted_at IS NULL;
