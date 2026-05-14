-- M3 Phase A: observability for "0 facts" runs. The pre-M3 `reflector_runs`
-- row recorded thoughts-processed / facts-committed / review-queue counts +
-- run-level error, but not per-thought extractor failures. Result: operator
-- couldn't tell from the runs table alone whether "0 facts" meant "no facts
-- to find" or "extractor unreachable for every call." This column closes the
-- gap.
ALTER TABLE reflector_runs
  ADD COLUMN n_extractor_failures INT NOT NULL DEFAULT 0;
