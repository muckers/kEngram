//! engram-storage: sqlx-backed repository functions.
//!
//! The `Embedder` trait is the only place we hide a backend choice behind a
//! trait — storage is concrete sqlx + Postgres. CLAUDE.md rule: compile-time
//! `sqlx::query!` everywhere except where pgvector's vector binding gets in
//! the way of the macro (currently: only `insert_embedding`).

use engram_core::{Embedding, EmbeddingModel, Metadata, Scope, ScopeError, Source, SourceError, Thought, ThoughtId};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub mod target {
    //! `embeddings.target_kind` enum-as-string. Matches the CHECK constraint
    //! on the column.
    pub const THOUGHT: &str = "thought";
    pub const ARTIFACT_CHUNK: &str = "artifact_chunk";
    pub const FACT: &str = "fact";
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("invalid scope decoded from database: {0}")]
    InvalidScope(#[from] ScopeError),

    #[error("invalid source decoded from database: {0}")]
    InvalidSource(#[from] SourceError),
}

/// Inputs for inserting a new thought. Borrowing keeps the call cheap.
#[derive(Debug, Clone, Copy)]
pub struct NewThought<'a> {
    pub scope: &'a Scope,
    pub content: &'a str,
    pub source: &'a Source,
    pub metadata: &'a Metadata,
}

/// What the DB tells us after a thought is inserted.
#[derive(Debug, Clone)]
pub struct InsertedThought {
    pub id: ThoughtId,
    pub created_at: OffsetDateTime,
}

/// Insert a thought. The database generates `id` and `created_at`.
pub async fn insert_thought(
    pool: &PgPool,
    t: NewThought<'_>,
) -> Result<InsertedThought, StorageError> {
    let row = sqlx::query!(
        r#"
        INSERT INTO thoughts (scope, content, source, metadata)
        VALUES ($1, $2, $3, $4)
        RETURNING id, created_at
        "#,
        t.scope.as_str(),
        t.content,
        t.source.as_str(),
        t.metadata.as_value(),
    )
    .fetch_one(pool)
    .await?;

    Ok(InsertedThought {
        id: ThoughtId::from(row.id),
        created_at: row.created_at,
    })
}

/// Insert an embedding row tied to some target (thought / artifact_chunk / fact).
///
/// Uses `sqlx::query` (runtime-checked) rather than the macro because pgvector's
/// `Vector` type is awkward to bind through `query!` — the macro can't infer
/// the column type from the schema alone. The query is still parameterised, so
/// no injection risk.
pub async fn insert_embedding(
    pool: &PgPool,
    target_kind: &'static str,
    target_id: Uuid,
    model: &EmbeddingModel,
    vector: Vec<f32>,
) -> Result<(), StorageError> {
    let pgv = pgvector::Vector::from(vector);
    sqlx::query(
        r#"
        INSERT INTO embeddings (target_kind, target_id, model_id, model_version, vector)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(target_kind)
    .bind(target_id)
    .bind(&model.id)
    .bind(1_i32) // model_version: bumped only when the same model_id changes its meaning
    .bind(pgv)
    .execute(pool)
    .await?;
    Ok(())
}

/// Convenience: insert an embedding tied to a thought, taking the engram-core
/// `Embedding` wrapper.
pub async fn insert_thought_embedding(
    pool: &PgPool,
    thought_id: ThoughtId,
    embedding: &Embedding,
) -> Result<(), StorageError> {
    insert_embedding(
        pool,
        target::THOUGHT,
        thought_id.into_uuid(),
        &embedding.model,
        embedding.vector.clone(),
    )
    .await
}

/// Look up a thought by id. Returns `None` if not found.
pub async fn fetch_thought(
    pool: &PgPool,
    id: ThoughtId,
) -> Result<Option<Thought>, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT id, scope, content, source, created_at, metadata
        FROM thoughts
        WHERE id = $1
        "#,
        id.into_uuid(),
    )
    .fetch_optional(pool)
    .await?;

    let Some(r) = row else {
        return Ok(None);
    };

    Ok(Some(Thought {
        id: ThoughtId::from(r.id),
        scope: Scope::new(r.scope)?,
        content: r.content,
        source: Source::new(r.source)?,
        created_at: r.created_at,
        metadata: Metadata::from(r.metadata),
    }))
}

/// True if an embedding exists for the given thought under the given model.
pub async fn thought_has_embedding(
    pool: &PgPool,
    id: ThoughtId,
    model: &EmbeddingModel,
) -> Result<bool, StorageError> {
    let row = sqlx::query!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM embeddings
            WHERE target_kind = 'thought' AND target_id = $1 AND model_id = $2
        ) AS "exists!"
        "#,
        id.into_uuid(),
        model.id,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.exists)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::{EmbeddingModel, Metadata, Scope, Source};
    use serde_json::json;

    fn new_thought<'a>(scope: &'a Scope, source: &'a Source, metadata: &'a Metadata) -> NewThought<'a> {
        NewThought {
            scope,
            content: "remember this",
            source,
            metadata,
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_thought_returns_id_and_timestamp(pool: PgPool) {
        let scope = Scope::new("work").unwrap();
        let source = Source::new("manual").unwrap();
        let metadata = Metadata::from(json!({"client_name": "test"}));

        let inserted = insert_thought(&pool, new_thought(&scope, &source, &metadata))
            .await
            .unwrap();

        // ID is non-nil, created_at is recent
        assert_ne!(*inserted.id.as_uuid(), Uuid::nil());
        let now = OffsetDateTime::now_utc();
        let drift = (now - inserted.created_at).whole_seconds().abs();
        assert!(drift < 10, "created_at not within 10s of now: drift={drift}s");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fetch_thought_returns_inserted_row(pool: PgPool) {
        let scope = Scope::new("personal").unwrap();
        let source = Source::new("agent:claude-code").unwrap();
        let metadata = Metadata::from(json!({"session_id": "abc"}));

        let inserted = insert_thought(&pool, new_thought(&scope, &source, &metadata))
            .await
            .unwrap();

        let fetched = fetch_thought(&pool, inserted.id).await.unwrap().unwrap();

        assert_eq!(fetched.id, inserted.id);
        assert_eq!(fetched.scope, scope);
        assert_eq!(fetched.content, "remember this");
        assert_eq!(fetched.source, source);
        assert_eq!(fetched.metadata, metadata);
        assert_eq!(fetched.created_at, inserted.created_at);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fetch_thought_returns_none_when_missing(pool: PgPool) {
        let id = ThoughtId::new();
        let result = fetch_thought(&pool, id).await.unwrap();
        assert!(result.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_embedding_persists_row(pool: PgPool) {
        let scope = Scope::default();
        let source = Source::new("manual").unwrap();
        let metadata = Metadata::empty();
        let inserted = insert_thought(&pool, new_thought(&scope, &source, &metadata))
            .await
            .unwrap();

        let model = EmbeddingModel::bge_m3();
        let vector = vec![0.0_f32; 1024];
        insert_embedding(
            &pool,
            target::THOUGHT,
            inserted.id.into_uuid(),
            &model,
            vector,
        )
        .await
        .unwrap();

        assert!(thought_has_embedding(&pool, inserted.id, &model).await.unwrap());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn thought_has_embedding_is_false_for_unembedded(pool: PgPool) {
        let scope = Scope::default();
        let source = Source::new("manual").unwrap();
        let metadata = Metadata::empty();
        let inserted = insert_thought(&pool, new_thought(&scope, &source, &metadata))
            .await
            .unwrap();

        let model = EmbeddingModel::bge_m3();
        assert!(!thought_has_embedding(&pool, inserted.id, &model).await.unwrap());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_thought_embedding_convenience_works(pool: PgPool) {
        let scope = Scope::default();
        let source = Source::new("manual").unwrap();
        let metadata = Metadata::empty();
        let inserted = insert_thought(&pool, new_thought(&scope, &source, &metadata))
            .await
            .unwrap();

        let model = EmbeddingModel::bge_m3();
        let embedding = Embedding::new(model.clone(), vec![0.5_f32; 1024]).unwrap();
        insert_thought_embedding(&pool, inserted.id, &embedding)
            .await
            .unwrap();

        assert!(thought_has_embedding(&pool, inserted.id, &model).await.unwrap());
    }
}
