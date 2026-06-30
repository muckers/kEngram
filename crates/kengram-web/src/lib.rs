//! kEngram M8 — read-only human web surface.
//!
//! A second *head* over the same retrieval core that backs `/mcp`: this crate
//! exposes read-only `/api/*` JSON routes (and, from Phase 3, server-rendered
//! HTML) so a human can search and visualize the corpus in a browser. It reuses
//! the transport-agnostic orchestrators in `kengram-mcp` (`search`, `relate`)
//! and their canonical JSON mappers, so `/api/*` returns byte-identical JSON to
//! the MCP tools. It never mutates — `psql` remains the write/admin interface
//! and `/mcp` the agent interface.
//!
//! `kengram-cli::run_serve` merges [`router`] onto its axum app when
//! `[web].enabled` is set. See `docs/milestones/m8-human-read-surface.md`.

mod api;
mod error;
mod host_guard;

pub use error::ApiError;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use kengram_core::Embedder;
use kengram_embed::Reranker;
use sqlx::PgPool;

/// Shared state for the web router. Holds the same handles the MCP server uses
/// (`pool` / `embedder` / `reranker`) plus the operator `allowed_hosts` list for
/// the Host-header guard. Cheap to clone (`PgPool` and `Arc` are shared).
#[derive(Clone)]
pub struct WebState {
    pub pool: PgPool,
    pub embedder: Arc<dyn Embedder>,
    pub reranker: Option<Arc<dyn Reranker>>,
    pub allowed_hosts: Vec<String>,
}

/// Build the read-only web router (read-only `/api/*` JSON for now; SSR pages
/// land in Phase 3). The Host-header guard is applied to every route, mirroring
/// the rmcp `allowed_hosts` check that protects `/mcp`.
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/api/search", get(api::search))
        .route("/api/recent", get(api::recent))
        .route("/api/scopes", get(api::scopes))
        .route("/api/thoughts/{id}", get(api::thought))
        .route("/api/thoughts/{id}/related", get(api::related))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            host_guard::guard,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use kengram_core::{Scope, Source};
    use kengram_embed::FakeEmbedder;
    use kengram_mcp::{CaptureRequest, capture, drain_pending_embeddings};
    use serde_json::Value;
    use tower::ServiceExt; // for `oneshot`

    const MODEL_ID: &str = "bge-m3:1024";

    fn state(pool: PgPool, allowed_hosts: Vec<String>) -> WebState {
        WebState {
            pool,
            embedder: Arc::new(FakeEmbedder::new()),
            reranker: None,
            allowed_hosts,
        }
    }

    /// Fire a GET at the router with a given Host header; return (status, body-json).
    async fn get(state: WebState, uri: &str, host: &str) -> (StatusCode, Value) {
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, json)
    }

    async fn cap(pool: &PgPool, content: &str, scope: &str) -> String {
        capture(
            pool,
            MODEL_ID,
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
        .to_string()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn disallowed_host_returns_403(pool: PgPool) {
        // Empty allowlist = loopback only; a non-loopback Host is rejected.
        let (status, _) = get(state(pool, vec![]), "/api/scopes", "evil.example.com").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn empty_query_returns_400(pool: PgPool) {
        let (status, body) = get(state(pool, vec![]), "/api/search", "localhost").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.get("error").is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn bad_uuid_returns_400(pool: PgPool) {
        let (status, _) = get(state(pool, vec![]), "/api/thoughts/not-a-uuid", "localhost").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn unknown_thought_returns_404(pool: PgPool) {
        let uri = "/api/thoughts/00000000-0000-0000-0000-000000000000";
        let (status, _) = get(state(pool, vec![]), uri, "localhost").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn search_round_trip_returns_200(pool: PgPool) {
        cap(
            &pool,
            "the peregrine falcon stoops at terminal velocity",
            "web.test",
        )
        .await;
        // Embed the pending thought so the vector leg can match.
        drain_pending_embeddings(&pool, &FakeEmbedder::new(), 16)
            .await
            .unwrap();
        let (status, body) = get(
            state(pool, vec![]),
            "/api/search?q=peregrine%20falcon",
            "localhost",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("results").and_then(Value::as_array).is_some());
        assert!(body.get("vector_search_available").is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn scopes_after_capture_returns_200(pool: PgPool) {
        cap(&pool, "a thought in a scope", "web.test").await;
        let (status, body) = get(state(pool, vec![]), "/api/scopes", "localhost").await;
        assert_eq!(status, StatusCode::OK);
        let scopes = body.get("scopes").and_then(Value::as_array).unwrap();
        assert!(
            scopes
                .iter()
                .any(|s| s.get("scope").and_then(Value::as_str) == Some("web.test"))
        );
    }
}
