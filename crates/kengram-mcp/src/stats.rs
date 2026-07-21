//! Corpus + storage telemetry: the `corpus_stats` orchestrator.
//!
//! A thin wrapper over [`kengram_storage::corpus_stats`] plus a canonical JSON
//! mapper, mirroring the `list_scopes` orchestrator/mapper pair in [`crate::search`].
//! It backs the CLI's `kengram stats` data source and the web UI's read-only
//! `/stats` page + `/api/stats` endpoint. There is deliberately **no** MCP tool
//! binding for this — it is an internal read orchestrator, not part of the MCP
//! surface. Byte counts are emitted as raw integers (humanization is a
//! presentation concern that lives in the consumer).

use kengram_storage::CorpusStats;
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;

use crate::search::ReadError;

/// Request for the `corpus_stats` orchestrator. Optional `scope_prefix` filters
/// only the `scopes` summary section (passed through to `list_scopes(prefix)`);
/// all other counts and byte totals are corpus-global.
#[derive(Debug, Clone, Default)]
pub struct CorpusStatsRequest {
    pub scope_prefix: Option<String>,
}

/// Response wrapping the storage-side [`CorpusStats`] verbatim. The struct is
/// already a plain data aggregate; the JSON mapper below is the single source of
/// truth for the wire shape.
#[derive(Debug, Clone)]
pub struct CorpusStatsResponse {
    pub stats: CorpusStats,
}

/// Aggregate corpus + storage telemetry. Wraps [`kengram_storage::corpus_stats`].
pub async fn corpus_stats(
    pool: &PgPool,
    request: CorpusStatsRequest,
) -> Result<CorpusStatsResponse, ReadError> {
    let scope_prefix = request.scope_prefix.filter(|s| !s.is_empty());
    let stats = kengram_storage::corpus_stats(pool, scope_prefix.as_deref()).await?;
    Ok(CorpusStatsResponse { stats })
}

/// Canonical JSON for a `corpus_stats` response. Byte counts are raw integers;
/// timestamps are RFC3339 (matching `list_scopes_response_json`).
pub fn corpus_stats_response_json(resp: &CorpusStatsResponse) -> serde_json::Value {
    let s = &resp.stats;

    let embeddings: Vec<serde_json::Value> = s
        .embeddings
        .iter()
        .map(|e| {
            serde_json::json!({
                "model_id": e.model_id,
                "model_version": e.model_version,
                "dimensions": e.dimensions,
                "count": e.count,
            })
        })
        .collect();

    let pairs = |v: &[(String, i64)]| -> Vec<serde_json::Value> {
        v.iter()
            .map(|(k, n)| serde_json::json!({ "key": k, "count": n }))
            .collect()
    };

    let scopes: Vec<serde_json::Value> = s
        .scopes
        .iter()
        .map(|sc| {
            serde_json::json!({
                "scope": sc.scope.as_str(),
                "thought_count": sc.thought_count,
                "first_activity_at": sc.first_activity_at.format(&Rfc3339).unwrap_or_default(),
                "last_activity_at": sc.last_activity_at.format(&Rfc3339).unwrap_or_default(),
            })
        })
        .collect();

    let tables: Vec<serde_json::Value> = s
        .storage
        .iter()
        .map(|t| {
            serde_json::json!({
                "table": t.table,
                "heap_bytes": t.heap_bytes,
                "indexes_bytes": t.indexes_bytes,
                "total_bytes": t.total_bytes,
            })
        })
        .collect();

    serde_json::json!({
        "thoughts": {
            "live": s.thoughts.live,
            "retracted": s.thoughts.retracted,
            "untagged": s.thoughts.untagged,
            "content_bytes_total": s.thoughts.content_bytes_total,
            "content_bytes_avg": s.thoughts.content_bytes_avg,
            "tags_bytes_total": s.thoughts.tags_bytes_total,
            "metadata_bytes_total": s.thoughts.metadata_bytes_total,
        },
        "embeddings": embeddings,
        "links": {
            "live": s.links.live,
            "soft_deleted": s.links.soft_deleted,
            "by_relation": pairs(&s.links.by_relation),
            "by_kind": pairs(&s.links.by_kind),
            "by_source": pairs(&s.links.by_source),
        },
        "queues": {
            "pending_embeddings": s.queues.pending_embeddings,
            "pending_tags": s.queues.pending_tags,
        },
        "scopes": scopes,
        "storage": {
            "database_total_bytes": s.database_total_bytes,
            "tables": tables,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureRequest, capture};
    use kengram_core::{Scope, Source};

    const MODEL_ID: &str = "bge-m3:1024";

    async fn cap(pool: &PgPool, content: &str, scope: &str) {
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
        .unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn corpus_stats_counts_and_json_shape(pool: PgPool) {
        cap(&pool, "the peregrine falcon stoops", "stats.test").await;
        cap(&pool, "a second thought in another scope", "stats.other").await;

        let resp = corpus_stats(&pool, CorpusStatsRequest::default())
            .await
            .unwrap();
        assert_eq!(resp.stats.thoughts.live, 2);
        assert_eq!(resp.stats.thoughts.retracted, 0);
        // Two distinct scopes captured.
        assert_eq!(resp.stats.scopes.len(), 2);

        let json = corpus_stats_response_json(&resp);
        assert_eq!(json["thoughts"]["live"], 2);
        assert!(json["links"]["by_relation"].is_array());
        assert!(json["queues"]["pending_embeddings"].is_i64());
        assert!(json["storage"]["database_total_bytes"].is_i64());
        assert!(json["storage"]["tables"].is_array());
        assert_eq!(json["scopes"].as_array().unwrap().len(), 2);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn scope_prefix_filters_scopes_section(pool: PgPool) {
        cap(&pool, "one", "stats.alpha").await;
        cap(&pool, "two", "other.beta").await;

        let resp = corpus_stats(
            &pool,
            CorpusStatsRequest {
                scope_prefix: Some("stats.".to_string()),
            },
        )
        .await
        .unwrap();
        // Corpus-global thought count stays global...
        assert_eq!(resp.stats.thoughts.live, 2);
        // ...but the scopes section is filtered to the prefix.
        assert_eq!(resp.stats.scopes.len(), 1);
        assert_eq!(resp.stats.scopes[0].scope.as_str(), "stats.alpha");
    }
}
