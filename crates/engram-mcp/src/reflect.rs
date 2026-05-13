//! `run_reflector_once` — single reflector pass driven by `tokio-cron-scheduler`
//! in the worker. Walks the LEFT-JOIN-IS-NULL unfacted-thoughts set, calls
//! the extractor per thought, routes each resulting fact to either `facts`
//! (committed) or `facts_review_queue` based on a configurable confidence
//! threshold. Per-thought extractor failures are soft (logged + counted +
//! continue); the thought re-appears in the next tick's unfacted set.
//!
//! Mirrors the `drain.rs` shape: a pure function over `&PgPool` + `&dyn
//! Extractor` + options, returning a `ReflectorReport`. The cron loop in
//! `engram-cli` wraps this call.

use engram_core::{ExtractionContext, Extractor};
use engram_storage::{NewFact, NewReviewRow, RunId};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;

/// Operator-tunable knobs for a reflector run. Deserialized straight from
/// `[reflector]` in `engram.toml`.
///
/// `enabled` is `false` by default: starting `engram worker` without an
/// `[extractor]` config or a running vLLM should be a no-op for the
/// reflector. The operator flips this to `true` once vLLM is up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReflectorOptions {
    pub enabled: bool,
    /// Cron expression. `tokio-cron-scheduler` accepts 6-field cron
    /// (sec min hour dom month dow). Default is 03:00 every day.
    pub schedule: String,
    pub scope_filter: Option<String>,
    pub max_thoughts_per_run: i64,
    pub max_facts_per_thought: usize,
    /// Confidence below this threshold routes the fact to
    /// `facts_review_queue` for operator review. At-or-above commits to
    /// `facts`. Single-band routing in Phase C; m2-facts-pipeline.md's
    /// three-band design (with a `flagged` column on `facts`) is deferred.
    pub review_queue_below: f32,
}

impl Default for ReflectorOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: "0 0 3 * * *".to_string(),
            scope_filter: None,
            max_thoughts_per_run: 1000,
            max_facts_per_thought: 8,
            review_queue_below: 0.7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReflectorReport {
    pub run_id: RunId,
    pub n_thoughts_processed: i32,
    pub n_facts_committed: i32,
    pub n_review_queue: i32,
    pub n_extractor_failures: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum ReflectorError {
    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),
}

/// One reflector pass. Opens a `reflector_runs` row, walks unfacted
/// thoughts, extracts + routes, closes the run with final counts.
pub async fn run_reflector_once(
    pool: &PgPool,
    extractor: &dyn Extractor,
    options: &ReflectorOptions,
) -> Result<ReflectorReport, ReflectorError> {
    let run_id = engram_storage::start_run(
        pool,
        extractor.model_id(),
        extractor.version(),
        options.scope_filter.as_deref(),
    )
    .await?;

    let thoughts = engram_storage::find_unfacted_thoughts(
        pool,
        options.scope_filter.as_deref(),
        options.max_thoughts_per_run,
    )
    .await?;

    let mut n_processed: i32 = 0;
    let mut n_committed: i32 = 0;
    let mut n_review: i32 = 0;
    let mut n_failures: i32 = 0;

    for thought in &thoughts {
        let ctx = ExtractionContext::new(thought.scope.clone(), options.max_facts_per_thought);
        let facts = match extractor.extract(thought, &ctx).await {
            Ok(facts) => facts,
            Err(err) => {
                // Per Q9: per-thought soft-fail. The unfacted thought
                // remains in the next tick's LEFT-JOIN-IS-NULL set.
                tracing::warn!(
                    run_id = %run_id,
                    thought_id = %thought.id,
                    error = %err,
                    transient = err.is_transient(),
                    "reflector: extractor failed; thought skipped this run",
                );
                n_failures += 1;
                n_processed += 1;
                continue;
            }
        };

        for fact in facts {
            // Skip degenerate empty statements (extractor edge case).
            if fact.statement.trim().is_empty() {
                continue;
            }
            let route = if fact.confidence < options.review_queue_below {
                Route::Review
            } else {
                Route::Commit
            };
            let res = match route {
                Route::Commit => engram_storage::insert_fact(
                    pool,
                    NewFact {
                        scope: &thought.scope,
                        statement: &fact.statement,
                        subject: fact.subject.as_deref(),
                        predicate: fact.predicate.as_deref(),
                        object: fact.object.as_deref(),
                        source_thought_id: thought.id,
                        extractor_model: extractor.model_id(),
                        extractor_version: extractor.version(),
                        source_run_id: Some(run_id),
                        confidence: fact.confidence,
                    },
                )
                .await
                .map(|_| ()),
                Route::Review => engram_storage::insert_review_queue_row(
                    pool,
                    NewReviewRow {
                        statement: &fact.statement,
                        subject: fact.subject.as_deref(),
                        predicate: fact.predicate.as_deref(),
                        object: fact.object.as_deref(),
                        source_thought_id: thought.id,
                        extractor_model: extractor.model_id(),
                        extractor_version: extractor.version(),
                        source_run_id: Some(run_id),
                        confidence: fact.confidence,
                    },
                )
                .await
                .map(|_| ()),
            };
            match res {
                Ok(()) => match route {
                    Route::Commit => n_committed += 1,
                    Route::Review => n_review += 1,
                },
                Err(err) => {
                    tracing::error!(
                        run_id = %run_id,
                        thought_id = %thought.id,
                        statement = %fact.statement,
                        error = %err,
                        "reflector: failed to persist extracted fact",
                    );
                }
            }
        }
        n_processed += 1;
    }

    engram_storage::finish_run(pool, run_id, n_processed, n_committed, n_review, None).await?;

    Ok(ReflectorReport {
        run_id,
        n_thoughts_processed: n_processed,
        n_facts_committed: n_committed,
        n_review_queue: n_review,
        n_extractor_failures: n_failures,
    })
}

#[derive(Debug, Clone, Copy)]
enum Route {
    Commit,
    Review,
}

/// Re-evaluate already-facted thoughts and reconcile against the current
/// extractor. For each thought, walk the new extractions and:
///
/// - if `(subject, predicate, object, statement)` exactly matches an existing
///   active fact → **no-op** (this is the idempotency keystone — re-running
///   on a stable extractor produces the same fact set);
/// - if `(subject, predicate, object)` matches but statement differs →
///   insert the new fact and supersede the old one (the old row stays in
///   `facts` with `superseded_by` pointing at the new row, preserving the
///   audit trail);
/// - if no existing active fact has this triple → insert as a brand-new
///   fact on the thought.
///
/// Phase D **does not** subtract: existing active facts that the new
/// extractor *doesn't* reproduce stay active. Rationale: a single rerun
/// reflects model drift in how facts are stated, not in what the thought
/// says — subtractive logic risks losing real facts to sampling variance.
/// Operators can `correct_fact` such rows manually.
///
/// Per-thought extractor failures are soft (logged + counted + continue),
/// matching `run_reflector_once`'s Q9 behavior.
pub async fn run_reflector_rerun(
    pool: &PgPool,
    extractor: &dyn Extractor,
    options: &ReflectorOptions,
    since: Option<OffsetDateTime>,
) -> Result<ReflectorReport, ReflectorError> {
    let run_id = engram_storage::start_run(
        pool,
        extractor.model_id(),
        extractor.version(),
        options.scope_filter.as_deref(),
    )
    .await?;

    let thoughts = engram_storage::find_facted_thoughts(
        pool,
        options.scope_filter.as_deref(),
        since,
        options.max_thoughts_per_run,
    )
    .await?;

    let mut n_processed: i32 = 0;
    let mut n_committed: i32 = 0;
    let mut n_review: i32 = 0;
    let mut n_failures: i32 = 0;

    for thought in &thoughts {
        let ctx = ExtractionContext::new(thought.scope.clone(), options.max_facts_per_thought);
        let facts = match extractor.extract(thought, &ctx).await {
            Ok(facts) => facts,
            Err(err) => {
                tracing::warn!(
                    run_id = %run_id,
                    thought_id = %thought.id,
                    error = %err,
                    transient = err.is_transient(),
                    "reflector rerun: extractor failed; thought skipped this run",
                );
                n_failures += 1;
                n_processed += 1;
                continue;
            }
        };

        for fact in facts {
            if fact.statement.trim().is_empty() {
                continue;
            }
            let existing = engram_storage::find_matching_active_fact(
                pool,
                thought.id,
                fact.subject.as_deref(),
                fact.predicate.as_deref(),
                fact.object.as_deref(),
            )
            .await?;

            match existing {
                Some(ref e) if e.statement == fact.statement => {
                    // Exact match — idempotency keystone. No-op.
                }
                Some(existing_fact) => {
                    // (S,P,O) match but statement differs: supersede.
                    // Insert + supersede in the same transaction so a crash
                    // between writes can't orphan the old row.
                    let mut tx = pool.begin().await.map_err(engram_storage::StorageError::from)?;

                    let route = if fact.confidence < options.review_queue_below {
                        Route::Review
                    } else {
                        Route::Commit
                    };
                    match route {
                        Route::Commit => {
                            let new_id: uuid::Uuid = sqlx::query_scalar!(
                                r#"
                                INSERT INTO facts (
                                    scope, statement, subject, predicate, object,
                                    source_thought_id, extractor_model, extractor_version,
                                    source_run_id, confidence
                                )
                                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                                RETURNING id
                                "#,
                                thought.scope.as_str(),
                                fact.statement,
                                fact.subject,
                                fact.predicate,
                                fact.object,
                                thought.id.into_uuid(),
                                extractor.model_id(),
                                extractor.version(),
                                run_id.0,
                                fact.confidence,
                            )
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(engram_storage::StorageError::from)?;

                            sqlx::query!(
                                r#"
                                UPDATE facts
                                SET superseded_by = $2, superseded_at = NOW()
                                WHERE id = $1 AND superseded_at IS NULL
                                "#,
                                existing_fact.id,
                                new_id,
                            )
                            .execute(&mut *tx)
                            .await
                            .map_err(engram_storage::StorageError::from)?;

                            tx.commit().await.map_err(engram_storage::StorageError::from)?;
                            n_committed += 1;
                        }
                        Route::Review => {
                            // Low confidence revisions land in the review
                            // queue, not as superseding facts — the old
                            // (higher-confidence?) row stays active.
                            engram_storage::insert_review_queue_row(
                                &pool.clone(),
                                NewReviewRow {
                                    statement: &fact.statement,
                                    subject: fact.subject.as_deref(),
                                    predicate: fact.predicate.as_deref(),
                                    object: fact.object.as_deref(),
                                    source_thought_id: thought.id,
                                    extractor_model: extractor.model_id(),
                                    extractor_version: extractor.version(),
                                    source_run_id: Some(run_id),
                                    confidence: fact.confidence,
                                },
                            )
                            .await?;
                            n_review += 1;
                            // No supersede — the new low-confidence row is in the queue, not facts.
                            tx.rollback().await.map_err(engram_storage::StorageError::from)?;
                        }
                    }
                }
                None => {
                    // Brand-new fact for this thought. Route by confidence.
                    let route = if fact.confidence < options.review_queue_below {
                        Route::Review
                    } else {
                        Route::Commit
                    };
                    match route {
                        Route::Commit => {
                            engram_storage::insert_fact(
                                pool,
                                NewFact {
                                    scope: &thought.scope,
                                    statement: &fact.statement,
                                    subject: fact.subject.as_deref(),
                                    predicate: fact.predicate.as_deref(),
                                    object: fact.object.as_deref(),
                                    source_thought_id: thought.id,
                                    extractor_model: extractor.model_id(),
                                    extractor_version: extractor.version(),
                                    source_run_id: Some(run_id),
                                    confidence: fact.confidence,
                                },
                            )
                            .await?;
                            n_committed += 1;
                        }
                        Route::Review => {
                            engram_storage::insert_review_queue_row(
                                pool,
                                NewReviewRow {
                                    statement: &fact.statement,
                                    subject: fact.subject.as_deref(),
                                    predicate: fact.predicate.as_deref(),
                                    object: fact.object.as_deref(),
                                    source_thought_id: thought.id,
                                    extractor_model: extractor.model_id(),
                                    extractor_version: extractor.version(),
                                    source_run_id: Some(run_id),
                                    confidence: fact.confidence,
                                },
                            )
                            .await?;
                            n_review += 1;
                        }
                    }
                }
            }
        }
        n_processed += 1;
    }

    engram_storage::finish_run(pool, run_id, n_processed, n_committed, n_review, None).await?;

    Ok(ReflectorReport {
        run_id,
        n_thoughts_processed: n_processed,
        n_facts_committed: n_committed,
        n_review_queue: n_review,
        n_extractor_failures: n_failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{capture, CaptureRequest};
    use engram_core::{ExtractedFact, Scope, Source, ThoughtId};
    use engram_extract::{FakeBehavior, FakeExtractor};

    const TEST_MODEL_ID: &str = "bge-m3:1024";

    async fn cap(pool: &PgPool, content: &str, scope: &str) -> ThoughtId {
        capture(
            pool,
            TEST_MODEL_ID,
            CaptureRequest {
                content: content.to_string(),
                source: Source::new("test").unwrap(),
                scope: Some(Scope::new(scope).unwrap()),
                metadata: None,
            },
        )
        .await
        .unwrap()
        .thought_id
    }

    fn options(review_below: f32) -> ReflectorOptions {
        ReflectorOptions {
            enabled: true,
            schedule: "0 0 3 * * *".to_string(),
            scope_filter: None,
            max_thoughts_per_run: 100,
            max_facts_per_thought: 8,
            review_queue_below: review_below,
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn commits_high_confidence_facts(pool: PgPool) {
        let id = cap(&pool, "Engram uses pgvector", "global").await;
        let extractor = FakeExtractor::with_confidence(0.9);
        let report = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();
        assert_eq!(report.n_thoughts_processed, 1);
        assert_eq!(report.n_facts_committed, 1);
        assert_eq!(report.n_review_queue, 0);
        assert_eq!(report.n_extractor_failures, 0);

        let fact_count = sqlx::query!(
            r#"SELECT COUNT(*) AS "count!" FROM facts WHERE source_thought_id = $1"#,
            id.into_uuid(),
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count;
        assert_eq!(fact_count, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn routes_low_confidence_to_review_queue(pool: PgPool) {
        let id = cap(&pool, "vague claim", "global").await;
        let extractor = FakeExtractor::with_confidence(0.3);
        let report = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();
        assert_eq!(report.n_facts_committed, 0);
        assert_eq!(report.n_review_queue, 1);

        let fact_count = sqlx::query!(
            r#"SELECT COUNT(*) AS "count!" FROM facts WHERE source_thought_id = $1"#,
            id.into_uuid(),
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count;
        let review_count = sqlx::query!(
            r#"SELECT COUNT(*) AS "count!" FROM facts_review_queue WHERE source_thought_id = $1"#,
            id.into_uuid(),
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count;
        assert_eq!(fact_count, 0);
        assert_eq!(review_count, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn writes_source_run_id_on_committed_facts(pool: PgPool) {
        cap(&pool, "stamp me", "global").await;
        let extractor = FakeExtractor::with_confidence(0.85);
        let report = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();

        let row = sqlx::query!(
            r#"SELECT source_run_id FROM facts LIMIT 1"#
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.source_run_id, Some(report.run_id.0));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn skips_thought_when_extractor_fails(pool: PgPool) {
        cap(&pool, "unreachable extractor", "global").await;
        let extractor = FakeExtractor::always_failing(FakeBehavior::Unreachable);
        let report = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();
        assert_eq!(report.n_thoughts_processed, 1);
        assert_eq!(report.n_extractor_failures, 1);
        assert_eq!(report.n_facts_committed, 0);
        assert_eq!(report.n_review_queue, 0);

        let fact_count = sqlx::query!(r#"SELECT COUNT(*) AS "count!" FROM facts"#)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count;
        assert_eq!(fact_count, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn is_idempotent_on_rerun(pool: PgPool) {
        cap(&pool, "extract me once", "global").await;
        let extractor = FakeExtractor::with_confidence(0.9);

        let first = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();
        assert_eq!(first.n_facts_committed, 1);

        // Second run: the thought now has a fact, so it's excluded from
        // find_unfacted_thoughts and produces no new rows.
        let second = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();
        assert_eq!(second.n_thoughts_processed, 0);
        assert_eq!(second.n_facts_committed, 0);

        let fact_count = sqlx::query!(r#"SELECT COUNT(*) AS "count!" FROM facts"#)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count;
        assert_eq!(fact_count, 1, "rerun must not duplicate facts");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn updates_run_counts(pool: PgPool) {
        for i in 0..3 {
            cap(&pool, &format!("t-{i}"), "global").await;
        }
        let extractor = FakeExtractor::with_confidence(0.9);
        let report = run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();

        let row = sqlx::query!(
            r#"SELECT n_thoughts_processed, n_facts_committed, n_review_queue, finished_at, error
               FROM reflector_runs WHERE id = $1"#,
            report.run_id.0,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.n_thoughts_processed, 3);
        assert_eq!(row.n_facts_committed, 3);
        assert_eq!(row.n_review_queue, 0);
        assert!(row.finished_at.is_some());
        assert!(row.error.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn scope_filter_only_processes_in_scope(pool: PgPool) {
        cap(&pool, "in scope", "work").await;
        cap(&pool, "out of scope", "personal").await;

        let extractor = FakeExtractor::with_confidence(0.9);
        let mut opts = options(0.7);
        opts.scope_filter = Some("work".to_string());
        let report = run_reflector_once(&pool, &extractor, &opts).await.unwrap();
        assert_eq!(report.n_thoughts_processed, 1);
        assert_eq!(report.n_facts_committed, 1);

        // The personal thought is still unfacted.
        let remaining =
            engram_storage::find_unfacted_thoughts(&pool, None, 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].scope.as_str(), "personal");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn explicit_facts_override_persists_subject_predicate_object(pool: PgPool) {
        cap(&pool, "anchor", "global").await;
        let extractor = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "Engram uses pgvector".into(),
            subject: Some("Engram".into()),
            predicate: Some("uses".into()),
            object: Some("pgvector".into()),
            confidence: 0.95,
        }]);
        run_reflector_once(&pool, &extractor, &options(0.7))
            .await
            .unwrap();

        let row = sqlx::query!(
            r#"SELECT statement, subject, predicate, object FROM facts LIMIT 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.statement, "Engram uses pgvector");
        assert_eq!(row.subject.as_deref(), Some("Engram"));
        assert_eq!(row.predicate.as_deref(), Some("uses"));
        assert_eq!(row.object.as_deref(), Some("pgvector"));
    }

    // -- M2 Phase D: run_reflector_rerun ----------------------------------

    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_no_ops_on_exact_match(pool: PgPool) {
        // First commit a fact via the normal reflector, then rerun the
        // same extractor → exact match → no change.
        cap(&pool, "stable thought", "global").await;
        let extractor = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "stable fact".into(),
            subject: Some("S".into()),
            predicate: Some("P".into()),
            object: Some("O".into()),
            confidence: 0.9,
        }]);
        run_reflector_once(&pool, &extractor, &options(0.7)).await.unwrap();
        let before = sqlx::query!(r#"SELECT COUNT(*) AS "n!" FROM facts"#).fetch_one(&pool).await.unwrap().n;

        // Rerun with the same extractor → exact (S,P,O,statement) match.
        let report = run_reflector_rerun(&pool, &extractor, &options(0.7), None).await.unwrap();
        let after = sqlx::query!(r#"SELECT COUNT(*) AS "n!" FROM facts"#).fetch_one(&pool).await.unwrap().n;
        assert_eq!(before, after, "exact match must produce zero new rows");
        assert_eq!(report.n_facts_committed, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_supersedes_when_statement_changes_for_same_triple(pool: PgPool) {
        cap(&pool, "drifting thought", "global").await;
        // First pass: produce the old statement.
        let v1 = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "old wording".into(),
            subject: Some("S".into()),
            predicate: Some("P".into()),
            object: Some("O".into()),
            confidence: 0.9,
        }]);
        run_reflector_once(&pool, &v1, &options(0.7)).await.unwrap();

        // Rerun with an extractor that gives the same (S,P,O) but new statement.
        let v2 = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "new wording".into(),
            subject: Some("S".into()),
            predicate: Some("P".into()),
            object: Some("O".into()),
            confidence: 0.9,
        }]);
        run_reflector_rerun(&pool, &v2, &options(0.7), None).await.unwrap();

        // Active facts now: only the new one.
        let active = sqlx::query!(
            r#"SELECT statement FROM facts WHERE superseded_at IS NULL"#
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].statement, "new wording");

        // Audit: old row is still in `facts`, marked superseded, with superseded_by → new.
        let old = sqlx::query!(
            r#"SELECT id, superseded_by, superseded_at
               FROM facts WHERE statement = 'old wording'"#
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(old.superseded_at.is_some());
        assert!(old.superseded_by.is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_inserts_new_fact_when_no_triple_match(pool: PgPool) {
        cap(&pool, "growing thought", "global").await;
        let v1 = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "old fact".into(),
            subject: Some("A".into()),
            predicate: Some("rel".into()),
            object: Some("B".into()),
            confidence: 0.9,
        }]);
        run_reflector_once(&pool, &v1, &options(0.7)).await.unwrap();

        // Rerun with an extractor that produces a *different* (S,P,O).
        let v2 = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "additional insight".into(),
            subject: Some("X".into()),
            predicate: Some("rel".into()),
            object: Some("Y".into()),
            confidence: 0.9,
        }]);
        run_reflector_rerun(&pool, &v2, &options(0.7), None).await.unwrap();

        // Both should be active — the old one is not subtracted.
        let active = sqlx::query!(
            r#"SELECT statement FROM facts WHERE superseded_at IS NULL ORDER BY statement"#
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(active.len(), 2);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_run_twice_produces_identical_fact_set(pool: PgPool) {
        // m2-facts-pipeline.md success criterion #5: idempotency.
        cap(&pool, "anchor", "global").await;
        let extractor = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "stable".into(),
            subject: Some("S".into()),
            predicate: Some("P".into()),
            object: Some("O".into()),
            confidence: 0.9,
        }]);
        run_reflector_once(&pool, &extractor, &options(0.7)).await.unwrap();

        // First rerun.
        run_reflector_rerun(&pool, &extractor, &options(0.7), None).await.unwrap();
        let snap1 = sqlx::query!(
            r#"SELECT id, statement, superseded_at FROM facts ORDER BY id"#
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        // Second rerun.
        run_reflector_rerun(&pool, &extractor, &options(0.7), None).await.unwrap();
        let snap2 = sqlx::query!(
            r#"SELECT id, statement, superseded_at FROM facts ORDER BY id"#
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(snap1.len(), snap2.len(), "rerun count must not change after second rerun");
        for (a, b) in snap1.iter().zip(snap2.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.statement, b.statement);
            assert_eq!(
                a.superseded_at.is_some(),
                b.superseded_at.is_some(),
                "supersession state must be stable across reruns"
            );
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn rerun_respects_scope_and_since(pool: PgPool) {
        cap(&pool, "work thought", "work").await;
        cap(&pool, "personal thought", "personal").await;

        let v1 = FakeExtractor::with_confidence(0.9);
        run_reflector_once(&pool, &v1, &options(0.7)).await.unwrap();

        // Rerun scoped to "work" with a since cutoff in the past — should
        // process only the work thought.
        let v2 = FakeExtractor::with_facts(vec![ExtractedFact {
            statement: "rerun result".into(),
            subject: None, predicate: None, object: None,
            confidence: 0.9,
        }]);
        let mut opts = options(0.7);
        opts.scope_filter = Some("work".to_string());
        let since = OffsetDateTime::now_utc() - time::Duration::days(1);
        let report = run_reflector_rerun(&pool, &v2, &opts, Some(since)).await.unwrap();
        assert_eq!(report.n_thoughts_processed, 1);
    }
}
