-- no-transaction

-- Roll back the session guard introduced by migration 0014.

RESET max_parallel_maintenance_workers;
