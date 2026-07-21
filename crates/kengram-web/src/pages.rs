//! Server-rendered HTML pages (askama).
//!
//! `/` (search) and `/thought/{id}` render a thin shell that `app.js` hydrates
//! from `/api/*`. `/scopes` and `/scope/{name}` are rendered fully server-side
//! (simple lists; good first paint + a no-JS fallback).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use kengram_core::{Scope, TagKind};
use kengram_mcp::{
    CorpusStatsRequest, ListScopesRequest, RecentRequest, corpus_stats, list_scopes,
    recent_thoughts,
};
use serde::Deserialize;

use crate::WebState;
use crate::error::ApiError;

const PREVIEW_LEN: usize = 160;

/// Top-N scopes shown in the stats distribution (matches the CLI default).
const TOP_SCOPES: usize = 20;

/// Format a byte count with base-1024 units. Presentation-only mirror of the
/// CLI's `humanize_bytes` (`kengram-cli/src/main.rs`); the `/api/stats` JSON
/// keeps raw integers, so the two serve different layers.
fn humanize_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n_f = n as f64;
    if n_f < KB {
        format!("{n} B")
    } else if n_f < MB {
        format!("{:.0} KB", n_f / KB)
    } else if n_f < GB {
        format!("{:.1} MB", n_f / MB)
    } else {
        format!("{:.2} GB", n_f / GB)
    }
}

/// Bar width (0–100) for `value` relative to `max`. 0 when `max` is non-positive.
fn bar_pct(value: i64, max: i64) -> u32 {
    if max <= 0 {
        0
    } else {
        ((value.max(0) as f64 / max as f64) * 100.0).round() as u32
    }
}

/// Turn a `(label, count)` breakdown into proportion bars scaled to the group max.
fn count_bars(items: &[(String, i64)]) -> Vec<BarRow> {
    let max = items.iter().map(|(_, n)| *n).max().unwrap_or(0);
    items
        .iter()
        .map(|(k, n)| BarRow {
            label: k.clone(),
            value: n.to_string(),
            pct: bar_pct(*n, max),
            detail: String::new(),
        })
        .collect()
}

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

// ---- /stats --------------------------------------------------------------

/// A headline number card (label + big value + muted subtext).
struct StatCard {
    label: String,
    value: String,
    sub: String,
}

/// One proportion-bar row: a label, a right-aligned value, a fill width, and
/// optional muted detail text.
struct BarRow {
    label: String,
    value: String,
    pct: u32,
    detail: String,
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsPage {
    cards: Vec<StatCard>,
    embeddings: Vec<BarRow>,
    links_live: i64,
    links_soft_deleted: i64,
    by_relation: Vec<BarRow>,
    by_kind: Vec<BarRow>,
    by_source: Vec<BarRow>,
    scopes: Vec<BarRow>,
    scopes_total: usize,
    scopes_hidden: usize,
    tables: Vec<BarRow>,
    db_total: String,
}

pub(crate) async fn stats_page(State(st): State<WebState>) -> Result<Html<String>, ApiError> {
    let resp = corpus_stats(&st.pool, CorpusStatsRequest::default()).await?;
    let s = resp.stats;

    let t = &s.thoughts;
    let total_thoughts = t.live + t.retracted;
    let retracted_pct = if total_thoughts > 0 {
        100 * t.retracted / total_thoughts
    } else {
        0
    };
    let tagged = (t.live - t.untagged).max(0);
    let tagged_pct = if t.live > 0 { 100 * tagged / t.live } else { 0 };
    let embed_total: i64 = s.embeddings.iter().map(|e| e.count).sum();

    let cards = vec![
        StatCard {
            label: "Live thoughts".into(),
            value: t.live.to_string(),
            sub: format!("{} retracted ({}%)", t.retracted, retracted_pct),
        },
        StatCard {
            label: "Tagged".into(),
            value: format!("{tagged_pct}%"),
            sub: format!("{} untagged", t.untagged),
        },
        StatCard {
            label: "Content".into(),
            value: humanize_bytes(t.content_bytes_total),
            sub: format!("avg {}/thought", humanize_bytes(t.content_bytes_avg)),
        },
        StatCard {
            label: "Embeddings".into(),
            value: embed_total.to_string(),
            sub: format!("{} model(s)", s.embeddings.len()),
        },
        StatCard {
            label: "Links".into(),
            value: s.links.live.to_string(),
            sub: format!("{} soft-deleted", s.links.soft_deleted),
        },
        StatCard {
            label: "Queues".into(),
            value: (s.queues.pending_embeddings + s.queues.pending_tags).to_string(),
            sub: format!(
                "{} embed · {} tag pending",
                s.queues.pending_embeddings, s.queues.pending_tags
            ),
        },
        StatCard {
            label: "Database".into(),
            value: humanize_bytes(s.database_total_bytes),
            sub: format!("{} tables", s.storage.len()),
        },
    ];

    // Embeddings by model, bars scaled to the largest model's count.
    let emb_max = s.embeddings.iter().map(|e| e.count).max().unwrap_or(0);
    let embeddings = s
        .embeddings
        .iter()
        .map(|e| BarRow {
            label: e.model_id.clone(),
            value: e.count.to_string(),
            pct: bar_pct(e.count, emb_max),
            detail: format!("{}-dim, v{}", e.dimensions, e.model_version),
        })
        .collect();

    // Scope distribution: top-N by thought count.
    let scopes_total = s.scopes.len();
    let mut scope_rows: Vec<(String, i64, String)> = s
        .scopes
        .iter()
        .map(|sc| {
            (
                sc.scope.as_str().to_string(),
                sc.thought_count,
                sc.last_activity_at.date().to_string(),
            )
        })
        .collect();
    scope_rows.sort_by_key(|r| std::cmp::Reverse(r.1)); // descending by thought count
    let scope_max = scope_rows.first().map(|r| r.1).unwrap_or(0);
    let scopes: Vec<BarRow> = scope_rows
        .iter()
        .take(TOP_SCOPES)
        .map(|(name, n, last)| BarRow {
            label: name.clone(),
            value: n.to_string(),
            pct: bar_pct(*n, scope_max),
            detail: format!("last {last}"),
        })
        .collect();
    let scopes_hidden = scopes_total.saturating_sub(scopes.len());

    // On-disk tables, bars scaled to the whole-database size.
    let tables = s
        .storage
        .iter()
        .map(|tb| BarRow {
            label: tb.table.clone(),
            value: humanize_bytes(tb.total_bytes),
            pct: bar_pct(tb.total_bytes, s.database_total_bytes),
            detail: format!(
                "heap {}, indexes {}",
                humanize_bytes(tb.heap_bytes),
                humanize_bytes(tb.indexes_bytes)
            ),
        })
        .collect();

    Ok(render(StatsPage {
        cards,
        embeddings,
        links_live: s.links.live,
        links_soft_deleted: s.links.soft_deleted,
        by_relation: count_bars(&s.links.by_relation),
        by_kind: count_bars(&s.links.by_kind),
        by_source: count_bars(&s.links.by_source),
        scopes,
        scopes_total,
        scopes_hidden,
        tables,
        db_total: humanize_bytes(s.database_total_bytes),
    }))
}
