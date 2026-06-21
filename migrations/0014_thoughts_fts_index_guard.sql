-- no-transaction

-- Session guard for the live-table FTS index build in migration 0015.
--
-- `CREATE INDEX CONCURRENTLY` must be the only statement in its migration
-- file, so this session-level GUC lives in the immediately preceding
-- no-transaction migration. SQLx applies migrations on one connection; this
-- keeps the 0015 GIN build single-threaded and avoids Docker's 64MB /dev/shm
-- ceiling that already broke parallel HNSW builds.

SET max_parallel_maintenance_workers = 0;
