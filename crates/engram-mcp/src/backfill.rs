//! `embed_backfill` — find thoughts without an embedding row for the active
//! model and embed them inline. This is the M1 catchup mechanism for
//! captures that landed with `embedding_status: "pending"` (because the
//! embedder was down at capture time). M2 replaces inline backfill with the
//! worker draining a job queue, but the CLI subcommand stays around for
//! ad-hoc use.

use engram_core::{Embedder, Embedding, Thought, ThoughtId};
use sqlx::PgPool;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillReport {
    /// Number of unembedded thoughts the query found.
    pub found: usize,
    /// Number successfully embedded + persisted.
    pub embedded: usize,
    /// Number that failed during embed/persist. Each failure is logged with
    /// the thought_id and reason.
    pub failed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum BackfillError {
    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),
}

pub async fn embed_backfill(
    pool: &PgPool,
    embedder: &dyn Embedder,
    scope: Option<&str>,
    limit: i64,
) -> Result<BackfillReport, BackfillError> {
    let pending: Vec<Thought> =
        engram_storage::find_unembedded_thoughts(pool, embedder.model(), scope, limit).await?;

    let mut report = BackfillReport {
        found: pending.len(),
        ..Default::default()
    };

    for thought in pending {
        match embed_and_persist_one(pool, embedder, &thought).await {
            Ok(()) => report.embedded += 1,
            Err(err) => {
                tracing::warn!(
                    thought_id = %thought.id,
                    reason = ?err,
                    "backfill: skipping thought",
                );
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

#[derive(Debug)]
enum SingleError {
    Embedder(engram_core::EmbedderError),
    Embedding(engram_core::EmbeddingError),
    Storage(engram_storage::StorageError),
    EmptyEmbedderOutput,
}

impl std::fmt::Display for SingleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embedder(e) => write!(f, "embedder: {e}"),
            Self::Embedding(e) => write!(f, "embedding: {e}"),
            Self::Storage(e) => write!(f, "storage: {e}"),
            Self::EmptyEmbedderOutput => f.write_str("embedder returned no vectors"),
        }
    }
}

async fn embed_and_persist_one(
    pool: &PgPool,
    embedder: &dyn Embedder,
    thought: &Thought,
) -> Result<(), SingleError> {
    let texts = vec![thought.content.clone()];
    let mut vectors = embedder.embed(&texts).await.map_err(SingleError::Embedder)?;
    let vector = vectors.pop().ok_or(SingleError::EmptyEmbedderOutput)?;
    let embedding = Embedding::new(embedder.model().clone(), vector)
        .map_err(SingleError::Embedding)?;
    engram_storage::insert_thought_embedding(pool, ThoughtId::from(thought.id.into_uuid()), &embedding)
        .await
        .map_err(SingleError::Storage)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{capture, CaptureRequest};
    use engram_core::{EmbeddingModel, Scope, Source};
    use engram_embed::{FakeBehavior, FakeEmbedder};

    async fn cap_with(
        pool: &PgPool,
        embedder: &dyn Embedder,
        content: &str,
        scope: &str,
    ) -> ThoughtId {
        let resp = capture(
            pool,
            embedder,
            CaptureRequest {
                content: content.to_string(),
                source: Source::new("test").unwrap(),
                scope: Some(Scope::new(scope).unwrap()),
                metadata: None,
            },
        )
        .await
        .unwrap();
        resp.thought_id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn embeds_previously_pending_thoughts(pool: PgPool) {
        // Capture under a failing embedder → thoughts land as Pending.
        let bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Unreachable);
        let id_a = cap_with(&pool, &bad, "alpha", "global").await;
        let id_b = cap_with(&pool, &bad, "beta", "global").await;

        // Now back-fill with a working embedder.
        let good = FakeEmbedder::new();
        let report = embed_backfill(&pool, &good, None, 100).await.unwrap();
        assert_eq!(report.found, 2);
        assert_eq!(report.embedded, 2);
        assert_eq!(report.failed, 0);

        // Both thoughts are now Indexed.
        assert!(
            engram_storage::thought_has_embedding(&pool, id_a, good.model())
                .await
                .unwrap()
        );
        assert!(
            engram_storage::thought_has_embedding(&pool, id_b, good.model())
                .await
                .unwrap()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn skips_already_embedded(pool: PgPool) {
        let good = FakeEmbedder::new();
        // Capture with a working embedder → already Indexed.
        cap_with(&pool, &good, "already done", "global").await;

        let report = embed_backfill(&pool, &good, None, 100).await.unwrap();
        assert_eq!(report.found, 0);
        assert_eq!(report.embedded, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn respects_scope_filter(pool: PgPool) {
        let bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Unreachable);
        cap_with(&pool, &bad, "work-1", "work").await;
        cap_with(&pool, &bad, "work-2", "work").await;
        cap_with(&pool, &bad, "personal-1", "personal").await;

        let good = FakeEmbedder::new();
        let report = embed_backfill(&pool, &good, Some("work"), 100).await.unwrap();
        assert_eq!(report.found, 2);
        assert_eq!(report.embedded, 2);

        // The personal one is still pending.
        let remaining = engram_storage::find_unembedded_thoughts(&pool, good.model(), None, 100)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].content, "personal-1");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn respects_limit(pool: PgPool) {
        let bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Unreachable);
        for i in 0..5 {
            cap_with(&pool, &bad, &format!("t-{i}"), "global").await;
        }

        let good = FakeEmbedder::new();
        let report = embed_backfill(&pool, &good, None, 2).await.unwrap();
        assert_eq!(report.found, 2);
        assert_eq!(report.embedded, 2);

        // Three still pending after one limited backfill.
        let remaining = engram_storage::find_unembedded_thoughts(&pool, good.model(), None, 100)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 3);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn handles_embedder_failure_for_individual_thoughts(pool: PgPool) {
        let bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Unreachable);
        cap_with(&pool, &bad, "stays pending", "global").await;

        // Backfill with still-failing embedder → 0 embedded, 1 failed.
        let still_bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Timeout);
        let report = embed_backfill(&pool, &still_bad, None, 100).await.unwrap();
        assert_eq!(report.found, 1);
        assert_eq!(report.embedded, 0);
        assert_eq!(report.failed, 1);
    }
}
