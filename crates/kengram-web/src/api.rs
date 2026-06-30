//! Read-only `/api/*` JSON handlers.
//!
//! Each handler parses HTTP query/path params, calls the corresponding
//! transport-agnostic `kengram-mcp` orchestrator, and serializes through that
//! orchestrator's canonical JSON mapper — so `/api/*` returns byte-identical
//! JSON to the MCP tools. No handler mutates.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use kengram_core::{LinkDirection, RelationKind, Scope, ThoughtId};
use kengram_mcp::{
    GetRelatedThoughtsRequest, ListScopesRequest, RecentRequest, SearchRequest,
    get_related_thoughts, get_thought, get_thought_response_json, list_scopes,
    list_scopes_response_json, recent_response_json, recent_thoughts,
    related_thoughts_response_json, search_response_json, search_thoughts,
};
use serde::Deserialize;
use serde_json::Value;

use crate::WebState;
use crate::error::ApiError;

fn parse_scope(s: Option<String>) -> Result<Option<Scope>, ApiError> {
    match s {
        Some(s) => {
            Ok(Some(Scope::new(s).map_err(|e| {
                ApiError::bad_request(format!("invalid scope: {e}"))
            })?))
        }
        None => Ok(None),
    }
}

/// Split a comma-separated query value into trimmed, non-empty parts.
fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchParams {
    #[serde(default)]
    q: Option<String>,
    scope: Option<String>,
    scope_prefix: Option<String>,
    limit: Option<usize>,
    recency_half_life_days: Option<f32>,
    rerank: Option<bool>,
    candidate_pool: Option<usize>,
    /// JSON-encoded tags containment filter, e.g. `{"kind":"task"}`.
    tag_filter: Option<String>,
}

/// `GET /api/search` — hybrid search (vector + trigram, RRF, recency, optional
/// tag filter + rerank). Mirrors the `search_thoughts` MCP tool.
pub(crate) async fn search(
    State(st): State<WebState>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    let tag_filter = match p.tag_filter.as_deref() {
        Some(s) if !s.is_empty() => Some(
            serde_json::from_str::<Value>(s)
                .map_err(|e| ApiError::bad_request(format!("invalid tag_filter JSON: {e}")))?,
        ),
        _ => None,
    };
    let req = SearchRequest {
        query: p.q.unwrap_or_default(),
        scope: parse_scope(p.scope)?,
        scope_prefix: p.scope_prefix,
        limit: p.limit,
        recency_half_life_days: p.recency_half_life_days,
        rerank: p.rerank,
        candidate_pool: p.candidate_pool,
        tag_filter,
    };
    let resp = search_thoughts(&st.pool, st.embedder.as_ref(), st.reranker.as_deref(), req).await?;
    Ok(Json(search_response_json(&resp)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecentParams {
    scope: Option<String>,
    scope_prefix: Option<String>,
    limit: Option<usize>,
}

/// `GET /api/recent` — newest-first thoughts, optionally scope-filtered.
pub(crate) async fn recent(
    State(st): State<WebState>,
    Query(p): Query<RecentParams>,
) -> Result<Json<Value>, ApiError> {
    let req = RecentRequest {
        scope: parse_scope(p.scope)?,
        scope_prefix: p.scope_prefix,
        limit: p.limit,
    };
    let resp = recent_thoughts(&st.pool, req).await?;
    Ok(Json(recent_response_json(&resp)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScopesParams {
    prefix: Option<String>,
}

/// `GET /api/scopes` — scopes with counts + activity timestamps.
pub(crate) async fn scopes(
    State(st): State<WebState>,
    Query(p): Query<ScopesParams>,
) -> Result<Json<Value>, ApiError> {
    let resp = list_scopes(&st.pool, ListScopesRequest { prefix: p.prefix }).await?;
    Ok(Json(list_scopes_response_json(&resp)))
}

/// `GET /api/thoughts/{id}` — a single thought with provenance.
pub(crate) async fn thought(
    State(st): State<WebState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tid = ThoughtId::from_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid thought id: {e}")))?;
    let resp = get_thought(&st.pool, st.embedder.model(), tid).await?;
    Ok(Json(get_thought_response_json(&resp)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct RelatedParams {
    /// Comma-separated relation names to restrict to.
    relations: Option<String>,
    /// Comma-separated target kinds (`thought`/`entity`/`person`/`url`).
    target_kinds: Option<String>,
    /// `outbound` | `inbound` | `both` (default `both`).
    direction: Option<String>,
}

/// `GET /api/thoughts/{id}/related` — one-hop link-graph neighborhood. This is
/// the graph-expansion endpoint: the UI calls it once per node click.
pub(crate) async fn related(
    State(st): State<WebState>,
    Path(id): Path<String>,
    Query(p): Query<RelatedParams>,
) -> Result<Json<Value>, ApiError> {
    let thought_id = ThoughtId::from_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid thought id: {e}")))?;

    let relations = match p.relations.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            let mut parsed = Vec::new();
            for part in split_csv(s) {
                parsed.push(RelationKind::from_str(&part).map_err(|e| {
                    ApiError::bad_request(format!("invalid relation '{part}': {e}"))
                })?);
            }
            Some(parsed)
        }
        _ => None,
    };

    let target_kinds = p
        .target_kinds
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(split_csv);

    let direction = match p.direction.as_deref() {
        Some(s) if !s.trim().is_empty() => LinkDirection::from_str(s.trim())
            .map_err(|e| ApiError::bad_request(format!("invalid direction '{s}': {e}")))?,
        _ => LinkDirection::default(),
    };

    let resp = get_related_thoughts(
        &st.pool,
        GetRelatedThoughtsRequest {
            thought_id,
            relations,
            target_kinds,
            direction,
        },
    )
    .await?;
    Ok(Json(related_thoughts_response_json(&resp)))
}
