-- no-transaction

-- Session guard for the Gemini clean HNSW build in migration 0021.
--
-- `CREATE INDEX CONCURRENTLY` must remain isolated in its migration file.
-- SQLx applies migrations on one connection, so these session-level settings
-- carry into the following migration and avoid Docker's 64MB /dev/shm ceiling.

SET max_parallel_maintenance_workers = 0;
SET maintenance_work_mem = '32MB';
