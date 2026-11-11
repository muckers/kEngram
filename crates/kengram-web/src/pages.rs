//! Server-rendered HTML pages (askama).
//!
//! `/` (search) and `/thought/{id}` render a thin shell that `app.js` hydrates
//! from `/api/*`. `/scopes` (collapsible scope tree) and `/scope/{name}` (list)
//! are rendered fully server-side — good first paint + a no-JS fallback.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use kengram_core::{Scope, TagKind};
use kengram_mcp::{
    CorpusStatsRequest, ListScopesRequest, RecentRequest, ScopeSummaryHit, corpus_stats,
    list_scopes, recent_thoughts,
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
//
// `list_scopes` returns a flat list of *populated* scopes (exact-scope
// counts, live thoughts only). The page rebuilds the dotted hierarchy as a
// collapsible tree — the web mirror of the CLI's Scopes tree
// (`kengram-cli/src/main.rs`: `ScopeTreeNode` + `render_scopes_tree`), with
// the same synthesis / alphabetical / rollup semantics: the same
// box-drawing connectors (`├─`/`└─` + `│  ` slots) and the same absolute
// count columns (rollup right-aligned, own count in a parens column),
// collapsed by nested `<details>` instead of being flat text. Unlike the
// CLI the browser page shows *every* scope: no top-N truncation (it is the
// "see all scopes" target), and branch-only ancestors (`rjf`, `project`)
// newly appear as heads. Presentation-only: `/api/scopes` stays a flat
// array.

/// Trie node over dotted scope segments. `own_count` is `Some` only for
/// populated scopes; synthesized ancestors are branch nodes. Mirrors the
/// CLI's `ScopeTreeNode`.
#[derive(Default)]
struct ScopeNode {
    own_count: Option<i64>,
    own_last: Option<time::OffsetDateTime>,
    children: std::collections::BTreeMap<String, ScopeNode>,
}

impl ScopeNode {
    /// Thoughts in this node's whole subtree (own + descendants).
    fn rollup(&self) -> i64 {
        self.own_count.unwrap_or(0) + self.children.values().map(|c| c.rollup()).sum::<i64>()
    }

    /// Latest activity in the subtree (own + descendants).
    fn last(&self) -> Option<time::OffsetDateTime> {
        self.own_last
            .max(self.children.values().filter_map(|c| c.last()).max())
    }
}

/// Build the trie over `scope.split('.')` from the flat populated list
/// (same walk as the CLI's).
fn build_scope_trie(scopes: &[ScopeSummaryHit]) -> std::collections::BTreeMap<String, ScopeNode> {
    let mut roots: std::collections::BTreeMap<String, ScopeNode> =
        std::collections::BTreeMap::new();
    for s in scopes {
        let mut segs = s.scope.split('.');
        let first = segs.next().unwrap_or(s.scope.as_str());
        let mut node = roots.entry(first.to_string()).or_default();
        for seg in segs {
            node = node.children.entry(seg.to_string()).or_default();
        }
        node.own_count = Some(s.thought_count);
        node.own_last = Some(s.last_activity_at);
    }
    roots
}

/// Full scope path for a child segment; roots join onto the empty path
/// without a leading dot (mirrors the CLI's `scope_child_path`).
fn scope_child_path(path: &str, seg: &str) -> String {
    if path.is_empty() {
        seg.to_string()
    } else {
        format!("{path}.{seg}")
    }
}

/// The own count as a separate aligned column: `(280)` only when strictly
/// smaller than the rollup (CLI parity; leaves and branch-only heads show
/// the rollup alone).
fn scope_note_text(rollup: i64, own: Option<i64>) -> Option<String> {
    match own {
        Some(o) if o < rollup => Some(format!("({o})")),
        _ => None,
    }
}

/// One rendered subtree. `scope_node.html` recurses by calling `.render()`
/// on each child — askama 0.12's only supported template recursion
/// (recursive `{% macro %}`/`{% include %}` don't compile at this version;
/// 0.12 docs, "Recursive Structures"). Default (HTML) escaping stays ON so
/// every interpolated value is escaped — scope names are free-form text
/// (`Scope` only validates length); only each child's already-rendered
/// markup is marked `|safe`. (Deliberately *not* `escape = "none"` + `|e`:
/// at askama 0.12 `|e` follows the file-type setting and no-ops under
/// `none` — pinned by `render_escapes_scope_names`.)
#[derive(Template)]
#[template(path = "scope_node.html")]
struct ScopeNodeView {
    /// Display name: the node's last dotted segment (CLI parity).
    seg: String,
    /// Full dotted path → `/scope/{path}`.
    path: String,
    /// No thought sits at this exact scope (synthesized ancestor): the
    /// name is inert text, not a link — its exact-scope page would be empty.
    branch_only: bool,
    /// Box-drawing prefix (ancestor slots + own connector, e.g. `"│  └─ "`).
    /// Roots carry none — exactly the CLI's connector rule.
    conn: String,
    /// Whole-subtree rollup, right-aligned into the number column.
    count: String,
    /// Own count in parens, own column (see [`scope_note_text`]); empty
    /// when it would duplicate the rollup.
    note: String,
    /// Subtree-latest activity, `YYYY-MM-DD`.
    last: String,
    children: Vec<ScopeNodeView>,
}

/// Convert one trie node (and its whole subtree) to its view, precomputing
/// every display string per house style. `conn` is this node's rendered
/// prefix; `base` is the slot string this node's level contributes to its
/// descendants (`│  ` while it has later siblings, else spaces) — the
/// CLI's `cont` rule verbatim.
fn to_scope_node_view(path: &str, node: &ScopeNode, conn: String, base: String) -> ScopeNodeView {
    let n = node.children.len();
    let children = node
        .children
        .iter()
        .enumerate()
        .map(|(i, (seg, c))| {
            let last = i + 1 == n;
            to_scope_node_view(
                &scope_child_path(path, seg),
                c,
                format!("{base}{}", if last { "└─ " } else { "├─ " }),
                format!("{base}{}", if last { "   " } else { "│  " }),
            )
        })
        .collect();
    let rollup = node.rollup();
    ScopeNodeView {
        seg: path.rsplit('.').next().unwrap_or(path).to_string(),
        branch_only: node.own_count.is_none(),
        conn,
        count: rollup.to_string(),
        note: scope_note_text(rollup, node.own_count).unwrap_or_default(),
        last: node
            .last()
            .map(|d| d.date().to_string())
            .unwrap_or_default(),
        path: path.to_string(),
        children,
    }
}

/// Absolute column widths for the tree, computed over the whole forest the
/// way the CLI's render pass does it: one label column (connector slots +
/// the 1-char collapse toggle + name) so every rollup lands in one
/// right-aligned number column, then the number and parens columns. The
/// row is monospaced, so `ch` widths align exactly.
fn scope_tree_style(views: &[ScopeNodeView]) -> String {
    fn walk(vs: &[ScopeNodeView], acc: &mut (usize, usize, usize)) {
        for v in vs {
            acc.0 = acc
                .0
                .max(v.conn.chars().count() + 1 + v.seg.chars().count());
            acc.1 = acc.1.max(v.count.chars().count());
            acc.2 = acc.2.max(v.note.chars().count());
            walk(&v.children, acc);
        }
    }
    let (label, num, note) = {
        let mut acc = (0, 0, 0);
        walk(views, &mut acc);
        acc
    };
    format!("--scope-label-col: {label}ch; --scope-num-col: {num}ch; --scope-note-col: {note}ch;")
}

#[derive(Template)]
#[template(path = "scopes.html")]
struct ScopesPage {
    roots: Vec<ScopeNodeView>,
    /// Inline custom properties sizing the tree's label/number/parens
    /// columns (see [`scope_tree_style`]); interpolated onto `.scope-tree`.
    style: String,
}

pub(crate) async fn scopes_page(State(st): State<WebState>) -> Result<Html<String>, ApiError> {
    let resp = list_scopes(&st.pool, ListScopesRequest { prefix: None }).await?;
    let roots: Vec<ScopeNodeView> = build_scope_trie(&resp.scopes)
        .into_iter()
        .map(|(seg, node)| to_scope_node_view(&seg, &node, String::new(), String::new()))
        .collect();
    let style = scope_tree_style(&roots);
    Ok(render(ScopesPage { roots, style }))
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

#[cfg(test)]
mod scope_tree_tests {
    use super::*;

    fn hit(scope: &str, count: i64, day: i64) -> ScopeSummaryHit {
        ScopeSummaryHit {
            scope: scope.to_string(),
            thought_count: count,
            first_activity_at: time::OffsetDateTime::UNIX_EPOCH,
            last_activity_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(day),
        }
    }

    fn roots(hits: &[ScopeSummaryHit]) -> Vec<ScopeNodeView> {
        build_scope_trie(hits)
            .iter()
            .map(|(seg, node)| to_scope_node_view(seg, node, String::new(), String::new()))
            .collect()
    }

    /// Pre-order (alphabetical, per BTreeMap) collect of view segment names.
    fn segs(v: &ScopeNodeView) -> Vec<String> {
        let mut out = vec![v.seg.clone()];
        for c in &v.children {
            out.extend(segs(c));
        }
        out
    }

    #[test]
    fn synthesizes_branch_nodes_and_alphabetizes() {
        let r = roots(&[
            hit("zeroclaw", 1, 0),
            hit("rjf.professional.cto", 3, 0),
            hit("rjf.personal.fishing", 2, 0),
            hit("rjf.ham-radio", 4, 0),
        ]);
        let names: Vec<String> = r.iter().flat_map(segs).collect();
        assert_eq!(
            names,
            [
                "rjf",
                "ham-radio",
                "personal",
                "fishing",
                "professional",
                "cto",
                "zeroclaw"
            ]
        );
        assert!(r[0].branch_only); // rjf holds no thought itself
        assert!(r[0].children[1].branch_only); // personal
        assert!(!r[0].children[0].branch_only); // ham-radio is populated
    }

    #[test]
    fn rollup_shows_own_in_parens_when_smaller() {
        let r = roots(&[
            hit("rjf.tech", 280, 0),
            hit("rjf.tech.squrl", 11, 0),
            hit("rjf.tech.zed", 5, 0),
        ]);
        assert_eq!((r[0].count.as_str(), r[0].note.as_str()), ("296", "")); // branch root: rollup only
        let tech = &r[0].children[0];
        assert_eq!(
            (tech.seg.as_str(), tech.count.as_str(), tech.note.as_str()),
            ("tech", "296", "(280)")
        );
        // Children are alphabetical: squrl before zed.
        assert_eq!(
            (
                tech.children[0].seg.as_str(),
                tech.children[0].count.as_str(),
                tech.children[0].note.as_str()
            ),
            ("squrl", "11", "")
        );
        assert_eq!(
            (
                tech.children[1].seg.as_str(),
                tech.children[1].count.as_str(),
                tech.children[1].note.as_str()
            ),
            ("zed", "5", "")
        );
    }

    #[test]
    fn note_column_only_when_strictly_smaller() {
        assert_eq!(scope_note_text(10, Some(10)), None);
        assert_eq!(scope_note_text(10, None), None);
        assert_eq!(scope_note_text(10, Some(4)), Some("(4)".into()));
    }

    #[test]
    fn connectors_and_slots_match_the_cli() {
        let r = roots(&[
            hit("rjf.ham-radio", 1, 0),
            hit("rjf.tech.zed", 1, 0),
            hit("zeroclaw", 1, 0),
        ]);
        // Roots carry no connector (the CLI's depth-0 rule)…
        assert_eq!((r[0].seg.as_str(), r[0].conn.as_str()), ("rjf", ""));
        assert_eq!((r[1].seg.as_str(), r[1].conn.as_str()), ("zeroclaw", ""));
        // …siblings get ├─/└─ off the parent's slot base…
        assert_eq!(r[0].children[0].conn, "├─ "); // ham-radio
        assert_eq!(r[0].children[1].conn, "└─ "); // tech (last sibling)
        // …and a last sibling contributes spaces (not │) to its descendants.
        assert_eq!(r[0].children[1].children[0].conn, "   └─ "); // zed
    }

    #[test]
    fn last_is_subtree_max() {
        let r = roots(&[
            hit("rjf.tech", 1, 5),
            hit("rjf.tech.squrl", 1, 9),
            hit("rjf.zed", 1, 2),
        ]);
        assert_eq!(r[0].last, "1970-01-10"); // branch root: max over subtree
        assert_eq!(r[0].children[0].last, "1970-01-10"); // tech: own day 5 < child day 9
    }

    #[test]
    fn single_scope_builds_full_ancestor_chain() {
        // Unlike the CLI's single-line special case, the browser tree always
        // keeps the hierarchy (it renders all scopes, never a filtered subset).
        let r = roots(&[hit("rjf.quotes", 1, 0)]);
        assert_eq!(r.len(), 1);
        assert!(r[0].branch_only);
        assert_eq!(r[0].children.len(), 1);
        assert_eq!(r[0].children[0].seg, "quotes");
        assert!(!r[0].children[0].branch_only);
        assert!(roots(&[]).is_empty());
    }

    fn fixture_views() -> Vec<ScopeNodeView> {
        roots(&[
            hit("rjf.tech", 280, 1),
            hit("rjf.tech.squrl", 11, 3),
            hit("project.kengram", 77, 2),
        ])
    }

    /// Build the page exactly as the handler does (columns sized to the forest).
    fn page(roots: Vec<ScopeNodeView>) -> ScopesPage {
        let style = scope_tree_style(&roots);
        ScopesPage { roots, style }
    }

    #[test]
    fn column_style_sizes_to_the_forest() {
        // Widest label: squrl at "   └─ " + toggle + name = 6 + 1 + 5 = 12.
        // Largest rollup 291 (3 digits); widest note "(280)" (5).
        assert_eq!(
            scope_tree_style(&fixture_views()),
            "--scope-label-col: 12ch; --scope-num-col: 3ch; --scope-note-col: 5ch;"
        );
        assert_eq!(
            scope_tree_style(&[]),
            "--scope-label-col: 0ch; --scope-num-col: 0ch; --scope-note-col: 0ch;"
        );
    }

    #[test]
    fn render_uses_collapsible_details_nesting() -> Result<(), askama::Error> {
        let html = page(fixture_views()).render()?;
        assert!(html.contains("<details open class=\"scope-branch\">"));
        assert!(html.contains("<summary class=\"scope-row\">"));
        assert!(html.contains("<div class=\"scope-children\">"));
        assert!(html.contains("scope-leaf"));
        // CLI connectors render verbatim…
        assert!(html.contains("<span class=\"tree-conn\">└─ </span>"));
        assert!(html.contains("<span class=\"tree-conn\">   └─ </span>"));
        // …and rollup + own land in their own adjacent columns
        // (tech = own 280 + squrl 11; the rjf head is branch-only, no note).
        assert!(html.contains(
            "<span class=\"scope-rollup\">291</span><span class=\"scope-note\">(280)</span>"
        ));
        assert!(html.contains(
            "style=\"--scope-label-col: 12ch; --scope-num-col: 3ch; --scope-note-col: 5ch;\""
        ));
        Ok(())
    }

    #[test]
    fn render_links_exact_scopes_only() -> Result<(), askama::Error> {
        let html = page(fixture_views()).render()?;
        assert!(html.contains("<a class=\"scope-name\" href=\"/scope/rjf.tech\">tech</a>"));
        assert!(html.contains("<span class=\"scope-name\">rjf</span>"));
        assert!(!html.contains("href=\"/scope/rjf\"")); // branch heads stay inert
        Ok(())
    }

    #[test]
    fn render_escapes_scope_names() -> Result<(), askama::Error> {
        let r = roots(&[hit("evil.<script>alert(1)</script>", 1, 0)]);
        let html = page(r).render()?;
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>alert")); // no live injection
        Ok(())
    }
}
