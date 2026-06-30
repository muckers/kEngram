//! Server-rendered HTML pages (askama).
//!
//! `/` (search) and `/thought/{id}` render a thin shell that `app.js` hydrates
//! from `/api/*`. `/scopes` and `/scope/{name}` are rendered fully server-side
//! (simple lists; good first paint + a no-JS fallback).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use kengram_core::{Scope, TagKind};
use kengram_mcp::{ListScopesRequest, RecentRequest, list_scopes, recent_thoughts};
use serde::Deserialize;

use crate::WebState;
use crate::error::ApiError;

const PREVIEW_LEN: usize = 160;

/// Single-line, length-capped preview of a thought body.
fn preview(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > PREVIEW_LEN {
        let mut s: String = flat.chars().take(PREVIEW_LEN).collect();
        s.push('…');
        s
    } else {
        flat
    }
}

/// The tagger's `kind` as its wire string (e.g. `decision_record`), via serde.
fn kind_str(kind: &Option<TagKind>) -> String {
    kind.as_ref()
        .and_then(|k| serde_json::to_value(k).ok())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn render<T: Template>(t: T) -> Html<String> {
    Html(
        t.render()
            .unwrap_or_else(|e| format!("<pre>template error: {e}</pre>")),
    )
}

// ---- / (search) ----------------------------------------------------------

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage {
    q: String,
    scope: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SearchPageQuery {
    q: Option<String>,
    scope: Option<String>,
}

pub(crate) async fn search_page(Query(p): Query<SearchPageQuery>) -> Html<String> {
    render(SearchPage {
        q: p.q.unwrap_or_default(),
        scope: p.scope.unwrap_or_default(),
    })
}

// ---- /graph --------------------------------------------------------------

#[derive(Template)]
#[template(path = "graph.html")]
struct GraphPage {
    root: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct GraphPageQuery {
    root: Option<String>,
}

pub(crate) async fn graph_page(Query(p): Query<GraphPageQuery>) -> Html<String> {
    render(GraphPage {
        root: p.root.unwrap_or_default(),
    })
}

// ---- /thought/{id} -------------------------------------------------------

#[derive(Template)]
#[template(path = "thought.html")]
struct ThoughtPage {
    thought_id: String,
}

pub(crate) async fn thought_page(Path(id): Path<String>) -> Html<String> {
    render(ThoughtPage { thought_id: id })
}

// ---- /scopes -------------------------------------------------------------

struct ScopeRow {
    scope: String,
    thought_count: i64,
    last_activity: String,
}

#[derive(Template)]
#[template(path = "scopes.html")]
struct ScopesPage {
    scopes: Vec<ScopeRow>,
}

pub(crate) async fn scopes_page(State(st): State<WebState>) -> Result<Html<String>, ApiError> {
    let resp = list_scopes(&st.pool, ListScopesRequest { prefix: None }).await?;
    let scopes = resp
        .scopes
        .into_iter()
        .map(|s| ScopeRow {
            scope: s.scope,
            thought_count: s.thought_count,
            last_activity: s.last_activity_at.date().to_string(),
        })
        .collect();
    Ok(render(ScopesPage { scopes }))
}

// ---- /scope/{name} -------------------------------------------------------

struct ThoughtRow {
    id: String,
    preview: String,
    created_at: String,
    kind: String,
}

#[derive(Template)]
#[template(path = "scope.html")]
struct ScopePage {
    scope: String,
    thoughts: Vec<ThoughtRow>,
}

pub(crate) async fn scope_page(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Html<String>, ApiError> {
    let scope = Scope::new(name.clone())
        .map_err(|e| ApiError::bad_request(format!("invalid scope: {e}")))?;
    let resp = recent_thoughts(
        &st.pool,
        RecentRequest {
            scope: Some(scope),
            scope_prefix: None,
            limit: Some(100),
        },
    )
    .await?;
    let thoughts = resp
        .results
        .into_iter()
        .map(|t| ThoughtRow {
            id: t.id.to_string(),
            preview: preview(&t.content),
            created_at: t.created_at.date().to_string(),
            kind: kind_str(&t.tags.kind),
        })
        .collect();
    Ok(render(ScopePage {
        scope: name,
        thoughts,
    }))
}
