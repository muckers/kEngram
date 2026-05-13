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
}
