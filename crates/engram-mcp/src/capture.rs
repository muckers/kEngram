//! The `capture` operation: write a thought row, then try to embed it.
//!
//! Embedding is best-effort. If the embedder fails for any reason
//! (transient or otherwise), the thought is durable and the response
//! reports `embedding_status: "pending"`. A later backfill (Phase D
//! `engram embed-backfill`) catches up. This matches the answer to the
//! "Capture flow on embedding failure" open question in
//! `docs/milestones/m1-capture-and-search.md`.

use engram_core::{Embedder, Embedding, EmbeddingStatus, Metadata, Scope, Source, ThoughtId};
use sqlx::PgPool;

/// Hard upper bound on a single thought's content. Enforced before the DB
/// write so callers get a clean rejection.
pub const MAX_CONTENT_LEN: usize = 1_048_576; // 1 MiB

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub content: String,
    pub source: Source,
    pub scope: Option<Scope>,
    pub metadata: Option<Metadata>,
}

#[derive(Debug, Clone)]
pub struct CaptureResponse {
    pub thought_id: ThoughtId,
    pub embedding_status: EmbeddingStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("content must be non-empty")]
    EmptyContent,

    #[error("content is too long: {got} bytes (max {max})")]
    ContentTooLong { got: usize, max: usize },

    #[error("storage error: {0}")]
    Storage(#[from] engram_storage::StorageError),
}

/// Capture a thought. Always inserts the `thoughts` row before returning.
/// On embedding success, also inserts the `embeddings` row. On embedding
/// failure, logs and returns `EmbeddingStatus::Pending`.
pub async fn capture(
    pool: &PgPool,
    embedder: &dyn Embedder,
    request: CaptureRequest,
) -> Result<CaptureResponse, CaptureError> {
    // 1. Validate.
    if request.content.is_empty() {
        return Err(CaptureError::EmptyContent);
    }
    if request.content.len() > MAX_CONTENT_LEN {
        return Err(CaptureError::ContentTooLong {
            got: request.content.len(),
            max: MAX_CONTENT_LEN,
        });
    }

    let scope = request.scope.unwrap_or_default();
    let metadata = request.metadata.unwrap_or_default();

    // 2. Write the thought.
    let inserted = engram_storage::insert_thought(
        pool,
        engram_storage::NewThought {
            scope: &scope,
            content: &request.content,
            source: &request.source,
            metadata: &metadata,
        },
    )
    .await?;

    // 3. Try to embed + persist. Any failure leaves the thought as-is and
    //    surfaces `embedding_status: "pending"` to the caller.
    let embedding_status = match try_embed_and_persist(pool, embedder, inserted.id, &request.content).await {
        Ok(()) => EmbeddingStatus::Indexed,
        Err(err) => {
            // Log severity reflects whether this is "service hiccup" vs.
            // "your config is wrong, look at this." Either way the thought
            // is durable; the embedding gets caught up by backfill.
            if err.is_transient() {
                tracing::warn!(
                    thought_id = %inserted.id,
                    reason = ?err,
                    "embedding deferred: transient failure",
                );
            } else {
                tracing::error!(
                    thought_id = %inserted.id,
                    reason = ?err,
                    "embedding deferred: non-transient failure (likely misconfiguration)",
                );
            }
            EmbeddingStatus::Pending
        }
    };

    Ok(CaptureResponse {
        thought_id: inserted.id,
        embedding_status,
    })
}

/// Internal error type for the "try to embed + persist" step. Carries enough
/// info for capture to decide log severity.
#[derive(Debug)]
enum EmbedPersistError {
    Embedder(engram_core::EmbedderError),
    Embedding(engram_core::EmbeddingError),
    Storage(engram_storage::StorageError),
}

impl EmbedPersistError {
    fn is_transient(&self) -> bool {
        match self {
            Self::Embedder(e) => e.is_transient(),
            // Wrong dimensions = config drift, but still recoverable after
            // operator fixes config + runs backfill. Log loud, but treat as
            // a deferrable problem from capture's perspective.
            Self::Embedding(_) => false,
            // DB hiccup after a successful thought insert: deferrable.
            Self::Storage(_) => true,
        }
    }
}

impl std::fmt::Display for EmbedPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embedder(e) => write!(f, "embedder: {e}"),
            Self::Embedding(e) => write!(f, "embedding construction: {e}"),
            Self::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

async fn try_embed_and_persist(
    pool: &PgPool,
    embedder: &dyn Embedder,
    thought_id: ThoughtId,
    content: &str,
) -> Result<(), EmbedPersistError> {
    let texts = vec![content.to_string()];
    let mut vectors = embedder
        .embed(&texts)
        .await
        .map_err(EmbedPersistError::Embedder)?;

    let vector = vectors
        .pop()
        .ok_or_else(|| EmbedPersistError::Embedder(engram_core::EmbedderError::MalformedResponse(
            "embedder returned zero vectors for non-empty batch".into(),
        )))?;

    let embedding = Embedding::new(embedder.model().clone(), vector)
        .map_err(EmbedPersistError::Embedding)?;

    engram_storage::insert_thought_embedding(pool, thought_id, &embedding)
        .await
        .map_err(EmbedPersistError::Storage)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::EmbeddingModel;
    use engram_embed::{FakeBehavior, FakeEmbedder};
    use serde_json::json;

    fn req(content: &str, source: &str) -> CaptureRequest {
        CaptureRequest {
            content: content.to_string(),
            source: Source::new(source).unwrap(),
            scope: None,
            metadata: None,
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn writes_thought_and_embedding_returns_indexed(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let resp = capture(&pool, &embedder, req("first thought", "manual"))
            .await
            .unwrap();

        assert_eq!(resp.embedding_status, EmbeddingStatus::Indexed);

        let fetched = engram_storage::fetch_thought(&pool, resp.thought_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.content, "first thought");
        assert!(
            engram_storage::thought_has_embedding(&pool, resp.thought_id, &EmbeddingModel::bge_m3())
                .await
                .unwrap()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn embedder_timeout_returns_pending_thought_still_written(pool: PgPool) {
        let embedder = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Timeout);
        let resp = capture(&pool, &embedder, req("captured but unindexed", "manual"))
            .await
            .unwrap();

        assert_eq!(resp.embedding_status, EmbeddingStatus::Pending);

        // Thought is durable.
        let fetched = engram_storage::fetch_thought(&pool, resp.thought_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.content, "captured but unindexed");

        // No embedding row.
        assert!(
            !engram_storage::thought_has_embedding(&pool, resp.thought_id, &EmbeddingModel::bge_m3())
                .await
                .unwrap()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn embedder_unreachable_returns_pending(pool: PgPool) {
        let embedder = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Unreachable);
        let resp = capture(&pool, &embedder, req("hi", "manual")).await.unwrap();
        assert_eq!(resp.embedding_status, EmbeddingStatus::Pending);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn empty_content_returns_error(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let err = capture(&pool, &embedder, req("", "manual")).await.unwrap_err();
        assert!(matches!(err, CaptureError::EmptyContent));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn overlong_content_returns_error(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let big = "x".repeat(MAX_CONTENT_LEN + 1);
        let err = capture(&pool, &embedder, req(&big, "manual")).await.unwrap_err();
        assert!(matches!(err, CaptureError::ContentTooLong { got, max } if got > max));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn defaults_scope_to_global_when_missing(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let resp = capture(&pool, &embedder, req("hello", "manual")).await.unwrap();

        let fetched = engram_storage::fetch_thought(&pool, resp.thought_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.scope, Scope::global());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn defaults_metadata_to_empty_when_missing(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let resp = capture(&pool, &embedder, req("hello", "manual")).await.unwrap();

        let fetched = engram_storage::fetch_thought(&pool, resp.thought_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.metadata, Metadata::empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn preserves_scope_source_metadata(pool: PgPool) {
        let embedder = FakeEmbedder::new();
        let request = CaptureRequest {
            content: "remember this".to_string(),
            source: Source::new("agent:claude-code").unwrap(),
            scope: Some(Scope::new("work.tcgplayer").unwrap()),
            metadata: Some(Metadata::from(json!({"session_id": "abc", "tool_name": "TodoWrite"}))),
        };
        let resp = capture(&pool, &embedder, request.clone()).await.unwrap();

        let fetched = engram_storage::fetch_thought(&pool, resp.thought_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.scope, request.scope.unwrap());
        assert_eq!(fetched.source, request.source);
        assert_eq!(fetched.metadata, request.metadata.unwrap());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn pending_thought_can_later_be_backfilled(pool: PgPool) {
        // Capture with failing embedder → pending. Then re-embed with a working
        // embedder + insert manually. Verifies the data layout supports backfill.
        let bad = FakeEmbedder::always_failing(EmbeddingModel::bge_m3(), FakeBehavior::Timeout);
        let resp = capture(&pool, &bad, req("backfill me", "manual")).await.unwrap();
        assert_eq!(resp.embedding_status, EmbeddingStatus::Pending);

        let good = FakeEmbedder::new();
        let vectors = good.embed(&["backfill me".to_string()]).await.unwrap();
        let embedding = Embedding::new(good.model().clone(), vectors.into_iter().next().unwrap()).unwrap();
        engram_storage::insert_thought_embedding(&pool, resp.thought_id, &embedding)
            .await
            .unwrap();

        assert!(
            engram_storage::thought_has_embedding(&pool, resp.thought_id, &EmbeddingModel::bge_m3())
                .await
                .unwrap()
        );
    }
}
