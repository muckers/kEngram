//! `rescope` — re-file an existing thought under a new scope (M7.2).
//!
//! The supervised path behind `thoughts.scope` mutability: every move
//! appends a row to the append-only `scope_changes` audit table and
//! re-enqueues the thought's tag job (live thoughts only). Update, audit,
//! and requeue commit atomically — see `kengram_storage::rescope_thought`
//! / `rename_scope_subtree` for the transaction itself.
//!
//! This orchestrator is the shared seam (same discipline as `finalize`):
//! the CLI and the `rescope_thought` MCP tool behave identically because
//! both run the guards here — self-nesting rejection, target validation,
//! required non-blank `reason` (2026-11-08 operator ruling) — before any
//! storage call.

use kengram_core::{Scope, ScopeError, ThoughtId};
use sqlx::PgPool;

/// Default audit actor when the caller doesn't name one: the MCP surface
/// falls back to `mcp`, the CLI passes `manual`. Free-text per the
/// `thoughts.source` convention — the trail has to name *which* agent
/// moved a thought, which a transport tag (`cli`/`mcp`) couldn't.
pub const DEFAULT_ACTOR: &str = "mcp";

#[derive(Debug, Clone)]
pub struct RescopeThoughtRequest {
    pub thought_id: ThoughtId,
    /// Target scope, raw string — validated through `Scope::new` here.
    pub scope: String,
    /// Required and non-blank (2026-11-08 ruling: required everywhere —
    /// both CLI forms and this tool).
    pub reason: String,
    pub actor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescopeThoughtResponse {
    pub thought_id: ThoughtId,
    pub previous_scope: String,
    pub new_scope: String,
    /// `false` when already at target — no-op, nothing written. Distinct
    /// from `Err(NotFound)`, which the storage pre-read's `None` carries
    /// (a `scope <> target` predicate would collapse the two).
    pub changed: bool,
}

#[derive(Debug, Clone)]
pub struct RenameScopeRequest {
    /// Source subtree root, raw: matches `from` exactly and `from.*` on
    /// dotted boundaries.
    pub from: String,
    pub to: String,
    pub reason: String,
    pub actor: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameScopeResponse {
    /// One entry per matched row (retracted included). On a dry run this
    /// is the plan; on a real run, what was written.
    pub moved: Vec<kengram_storage::RescopeRow>,
    /// Matched *live* rows — what the tag queue gained (or already had).
    pub requeued: usize,
    /// `false` on dry runs and zero-match no-ops: nothing was committed.
    pub committed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RescopeError {
    #[error("thought not found: {0}")]
    NotFound(ThoughtId),

    #[error("invalid scope: {0}")]
    InvalidScope(#[from] ScopeError),

    #[error("reason is required — one sentence on why the filing changed")]
    MissingReason,

    #[error(
        "rename target `{to}` is the source scope or inside its subtree `{from}` — \
         such a rename re-matches its own output on every re-run"
    )]
    SelfNesting { from: String, to: String },

    #[error("storage error: {0}")]
    Storage(#[from] kengram_storage::StorageError),
}

fn guard_reason(reason: &str) -> Result<&str, RescopeError> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(RescopeError::MissingReason);
    }
    Ok(trimmed)
}

fn resolve_actor(actor: Option<&str>) -> String {
    actor
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_ACTOR.to_string())
}

/// Re-file one thought. `changed: false` (no-op, no audit row) when
/// already at target; `Err(NotFound)` for unknown ids — the same
/// no-op/did-work seam as `retract_thought`.
pub async fn rescope_thought(
    pool: &PgPool,
    tagger_model_id: &str,
    request: RescopeThoughtRequest,
) -> Result<RescopeThoughtResponse, RescopeError> {
    let reason = guard_reason(&request.reason)?;
    let target = Scope::new(request.scope)?;
    let actor = resolve_actor(request.actor.as_deref());

    let outcome = kengram_storage::rescope_thought(
        pool,
        request.thought_id,
        &target,
        &actor,
        reason,
        tagger_model_id,
    )
    .await?;

    let Some(previous_scope) = outcome.previous_scope else {
        return Err(RescopeError::NotFound(request.thought_id));
    };
    Ok(RescopeThoughtResponse {
        thought_id: request.thought_id,
        previous_scope,
        new_scope: outcome.new_scope,
        changed: outcome.changed,
    })
}

/// Dotted-boundary subtree rename — **CLI-only surface** (operator
/// taxonomy maintenance, not an agent workflow; see m7.2 contract §MCP
/// surface delta). Guards run before any storage call: a `to` equal to
/// or inside `from`'s subtree would re-match its own output on every
/// re-run and nest forever (`work.archive.archive.x`), fresh audit rows
/// per pass.
pub async fn rename_scope_subtree(
    pool: &PgPool,
    tagger_model_id: &str,
    request: RenameScopeRequest,
) -> Result<RenameScopeResponse, RescopeError> {
    let reason = guard_reason(&request.reason)?;
    Scope::new(&request.from)?;
    let target = Scope::new(&request.to)?;
    let actor = resolve_actor(request.actor.as_deref());

    let inside_subtree = request.to == request.from
        || request
            .to
            .starts_with(&format!("{}.", request.from.trim_end_matches('.')));
    if inside_subtree {
        return Err(RescopeError::SelfNesting {
            from: request.from.clone(),
            to: request.to.clone(),
        });
    }

    let outcome = kengram_storage::rename_scope_subtree(
        pool,
        &request.from,
        &target,
        &actor,
        reason,
        tagger_model_id,
        request.dry_run,
    )
    .await?;

    Ok(RenameScopeResponse {
        moved: outcome.moved,
        requeued: outcome.requeued,
        committed: outcome.committed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureRequest, capture};
    use kengram_core::{Source, ThoughtId as Tid};

    const TEST_EMBEDDER_MODEL_ID: &str = "bge-m3:1024";
    const TEST_TAGGER_ID: &str = "fake/v1";

    async fn cap(pool: &PgPool, content: &str, scope: &str) -> Tid {
        capture(
            pool,
            TEST_EMBEDDER_MODEL_ID,
            None,
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

    async fn audit_actor(pool: &PgPool, id: Tid) -> Option<String> {
        sqlx::query!(
            "SELECT actor FROM scope_changes WHERE thought_id = $1",
            Tid::into_uuid(id)
        )
        .fetch_optional(pool)
        .await
        .unwrap()
        .map(|r| r.actor)
    }

    fn req(
        thought_id: Tid,
        scope: &str,
        reason: &str,
        actor: Option<&str>,
    ) -> RescopeThoughtRequest {
        RescopeThoughtRequest {
            thought_id,
            scope: scope.to_string(),
            reason: reason.to_string(),
            actor: actor.map(str::to_string),
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn not_found_is_distinct_from_no_op(pool: PgPool) {
        // Criterion 1: a bad id yields a clean not-found *error*, while
        // already-at-target is an Ok(changed:false) no-op.
        let missing = ThoughtId::new();
        let err = rescope_thought(&pool, TEST_TAGGER_ID, req(missing, "work.bar", "x", None))
            .await
            .unwrap_err();
        assert!(matches!(err, RescopeError::NotFound(id) if id == missing));

        let id = cap(&pool, "placed", "work.bar").await;
        let resp = rescope_thought(&pool, TEST_TAGGER_ID, req(id, "work.bar", "rerun", None))
            .await
            .unwrap();
        assert!(!resp.changed);
        assert_eq!(resp.previous_scope.as_str(), "work.bar");
        // No-op wrote no audit row.
        assert!(audit_actor(&pool, id).await.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn actor_lands_verbatim_or_defaults_to_mcp(pool: PgPool) {
        // Criterion 7.
        let a = cap(&pool, "one", "global").await;
        let b = cap(&pool, "two", "global").await;
        rescope_thought(
            &pool,
            TEST_TAGGER_ID,
            req(a, "work.x", "naming", Some("agent:claude-code")),
        )
        .await
        .unwrap();
        rescope_thought(&pool, TEST_TAGGER_ID, req(b, "work.y", "naming", None))
            .await
            .unwrap();
        assert_eq!(
            audit_actor(&pool, a).await.as_deref(),
            Some("agent:claude-code")
        );
        assert_eq!(audit_actor(&pool, b).await.as_deref(), Some("mcp"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn blank_reason_is_rejected_everywhere(pool: PgPool) {
        let id = cap(&pool, "one", "global").await;
        let err = rescope_thought(&pool, TEST_TAGGER_ID, req(id, "work.x", "   ", None))
            .await
            .unwrap_err();
        assert!(matches!(err, RescopeError::MissingReason));

        let err = rename_scope_subtree(
            &pool,
            TEST_TAGGER_ID,
            RenameScopeRequest {
                from: "work".into(),
                to: "play".into(),
                reason: "".into(),
                actor: None,
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RescopeError::MissingReason));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn self_nesting_rejected_before_any_write(pool: PgPool) {
        // Criterion 4: `work → work.archive` (and `work → work`) fail up
        // front; nothing moved, nothing queued.
        let id = cap(&pool, "inside", "work.tools").await;
        for to in ["work", "work.archive", "work.archive.deep"] {
            let err = rename_scope_subtree(
                &pool,
                TEST_TAGGER_ID,
                RenameScopeRequest {
                    from: "work".into(),
                    to: to.into(),
                    reason: "taxonomy".into(),
                    actor: None,
                    dry_run: false,
                },
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, RescopeError::SelfNesting { .. }),
                "`work -> {to}` must be rejected"
            );
        }
        let scope = kengram_storage::fetch_thought(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .scope;
        assert_eq!(scope.as_str(), "work.tools");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn invalid_target_rejected(pool: PgPool) {
        let id = cap(&pool, "one", "global").await;
        let err = rescope_thought(&pool, TEST_TAGGER_ID, req(id, "", "x", None))
            .await
            .unwrap_err();
        assert!(matches!(err, RescopeError::InvalidScope(_)));
        let err = rescope_thought(
            &pool,
            TEST_TAGGER_ID,
            req(id, "t".repeat(257).as_str(), "x", None),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RescopeError::InvalidScope(_)));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn dry_run_plans_without_committing(pool: PgPool) {
        let id = cap(&pool, "one", "work.foo").await;
        let resp = rename_scope_subtree(
            &pool,
            TEST_TAGGER_ID,
            RenameScopeRequest {
                from: "work.foo".into(),
                to: "work.bar".into(),
                reason: "preview".into(),
                actor: None,
                dry_run: true,
            },
        )
        .await
        .unwrap();
        assert!(!resp.committed);
        assert_eq!(resp.moved.len(), 1);
        assert_eq!(resp.requeued, 1);
        let scope = kengram_storage::fetch_thought(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .scope;
        assert_eq!(scope.as_str(), "work.foo");
    }
}
