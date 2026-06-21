-- no-transaction

-- Roll back the FTS lexical index introduced by migration 0015.

DROP INDEX CONCURRENTLY IF EXISTS thoughts_content_fts_idx;
