//! Search composition primitives. Pure logic; storage and embedder I/O
//! live elsewhere.
//!
//! The hybrid retrieval pipeline is:
//!   1. Each retrieval leg (vector kNN, trigram similarity) returns a
//!      ranked `Vec<Hit>` of length ≤ top_k_per_leg.
//!   2. [`rrf_fuse`] combines the rankings into a single ordering by
//!      reciprocal rank fusion: `score(d) = Σ 1 / (k + rank_i(d))`.
//!   3. [`recency_boost`] multiplies each fused score by a half-life
//!      decay factor (`0.5^(age_days / half_life_days)`) and re-sorts.
//!
//! Default RRF `k = 60` matches the IR-literature standard. Default
//! recency half-life is 30 days.

use std::collections::HashMap;
use time::OffsetDateTime;

use crate::Thought;

pub const DEFAULT_RRF_K: f32 = 60.0;
pub const DEFAULT_RECENCY_HALF_LIFE_DAYS: f32 = 30.0;

/// A single retrieval hit. Storage layers return these from each leg; the
/// fusion layer accumulates and re-orders them.
///
/// `score` evolves through the search pipeline:
///   1. **Pre-fusion** (set by storage-layer leg fn): the raw score from
///      the producing leg — cosine similarity for the vector leg, trigram
///      word_similarity for the trigram leg.
///   2. **Post-fusion** ([`rrf_fuse`]): the RRF aggregate
///      `Σ 1/(k + rank_i)` over the legs that contributed.
///   3. **Post-recency** ([`recency_boost`]): the RRF aggregate multiplied
///      by a half-life decay factor.
///   4. **Post-rerank** (search orchestrator, when reranker is configured):
///      replaced with the cross-encoder's calibrated absolute score.
///
/// The optional per-leg fields preserve the **pre-fusion** signals so
/// consumers building thresholding logic (e.g. "only show hits where
/// `vector_score > 0.6`") can reach the raw signal even after the
/// pipeline has overwritten `score`. M3 Phase B step 2.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub thought: Thought,
    pub score: f32,
    /// Raw cosine similarity from the vector kNN leg. `None` when the hit
    /// did not appear in the vector leg (trigram-only match or vector leg
    /// unavailable).
    pub vector_score: Option<f32>,
    /// Raw `word_similarity` from the trigram leg. `None` when the hit did
    /// not appear in the trigram leg.
    pub trigram_score: Option<f32>,
    /// Calibrated absolute relevance score from the cross-encoder
    /// reranker. `None` when rerank was off, no reranker was configured,
    /// or the hit fell outside the reranked candidate pool.
    pub rerank_score: Option<f32>,
}

impl Hit {
    /// Construct a hit produced by the vector kNN leg — `score` is the raw
    /// cosine similarity, mirrored into the typed `vector_score` field.
    /// Trigram + rerank fields default to `None`.
    pub fn from_vector_leg(thought: Thought, cosine_similarity: f32) -> Self {
        Self {
            thought,
            score: cosine_similarity,
            vector_score: Some(cosine_similarity),
            trigram_score: None,
            rerank_score: None,
        }
    }

    /// Construct a hit produced by the trigram leg — `score` is the raw
    /// `word_similarity`, mirrored into `trigram_score`. Vector + rerank
    /// fields default to `None`.
    pub fn from_trigram_leg(thought: Thought, word_similarity: f32) -> Self {
        Self {
            thought,
            score: word_similarity,
            vector_score: None,
            trigram_score: Some(word_similarity),
            rerank_score: None,
        }
    }
}

/// Reciprocal Rank Fusion. Each ranking is taken in the order given.
/// Output is sorted by descending fused score. Per-leg score fields
/// (`vector_score`, `trigram_score`) are preserved across the fusion:
/// when both rankings carry the same thought, the leg-specific scores
/// from each input merge into the accumulator (an input's `Some(_)`
/// always wins over an existing `None`).
pub fn rrf_fuse(rankings: Vec<Vec<Hit>>, k: f32) -> Vec<Hit> {
    let mut acc: HashMap<crate::ThoughtId, Hit> = HashMap::new();

    for ranking in rankings {
        for (i, hit) in ranking.into_iter().enumerate() {
            let rank = (i + 1) as f32;
            let contribution = 1.0 / (k + rank);
            match acc.get_mut(&hit.thought.id) {
                Some(existing) => {
                    existing.score += contribution;
                    if existing.vector_score.is_none() {
                        existing.vector_score = hit.vector_score;
                    }
                    if existing.trigram_score.is_none() {
                        existing.trigram_score = hit.trigram_score;
                    }
                }
                None => {
                    let id = hit.thought.id;
                    let merged = Hit {
                        thought: hit.thought,
                        score: contribution,
                        vector_score: hit.vector_score,
                        trigram_score: hit.trigram_score,
                        rerank_score: None,
                    };
                    acc.insert(id, merged);
                }
            }
        }
    }

    let mut fused: Vec<Hit> = acc.into_values().collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused
}

/// Post-fusion recency boost. Multiplies each hit's score by
/// `0.5^(age_days / half_life_days)`, then re-sorts. A hit captured exactly
/// `half_life_days` ago is halved; one captured now is unchanged.
pub fn recency_boost(hits: &mut [Hit], half_life_days: f32, now: OffsetDateTime) {
    if half_life_days <= 0.0 {
        return; // disabled
    }
    for h in hits.iter_mut() {
        let age_secs = (now - h.thought.created_at).whole_seconds() as f32;
        let age_days = age_secs / 86_400.0;
        let factor = 0.5_f32.powf(age_days / half_life_days);
        h.score *= factor;
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Metadata, Scope, Source, ThoughtId};

    fn thought(id_seed: u128, content: &str, age_seconds: i64) -> Thought {
        let id = ThoughtId::from(uuid::Uuid::from_u128(id_seed));
        Thought {
            id,
            scope: Scope::default(),
            content: content.to_string(),
            source: Source::new("test").unwrap(),
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000 - age_seconds).unwrap(),
            metadata: Metadata::empty(),
        }
    }

    fn hit(t: Thought, score: f32) -> Hit {
        Hit {
            thought: t,
            score,
            vector_score: None,
            trigram_score: None,
            rerank_score: None,
        }
    }

    #[test]
    fn rrf_empty_rankings_returns_empty() {
        let out = rrf_fuse(vec![], DEFAULT_RRF_K);
        assert!(out.is_empty());
    }

    #[test]
    fn rrf_single_ranking_preserves_order_with_decreasing_scores() {
        let r = vec![
            hit(thought(1, "a", 0), 0.9),
            hit(thought(2, "b", 0), 0.5),
            hit(thought(3, "c", 0), 0.1),
        ];
        let out = rrf_fuse(vec![r], 60.0);
        assert_eq!(out.len(), 3);
        // First item gets 1/(60+1) = 0.0164; second 1/62 = 0.0161; third 1/63 = 0.0159.
        assert!(out[0].score > out[1].score);
        assert!(out[1].score > out[2].score);
        assert_eq!(out[0].thought.content, "a");
        assert_eq!(out[1].thought.content, "b");
        assert_eq!(out[2].thought.content, "c");
    }

    #[test]
    fn rrf_overlapping_hit_accumulates_score() {
        // 'a' appears at rank 1 in both rankings; 'b' only in the first.
        // Score(a) = 2 * 1/61 ≈ 0.0328; Score(b) = 1/62 ≈ 0.0161.
        let r1 = vec![hit(thought(1, "a", 0), 0.9), hit(thought(2, "b", 0), 0.5)];
        let r2 = vec![hit(thought(1, "a", 0), 0.8)];
        let out = rrf_fuse(vec![r1, r2], 60.0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].thought.content, "a");
        assert!((out[0].score - 2.0 / 61.0).abs() < 1e-6);
        assert_eq!(out[1].thought.content, "b");
        assert!((out[1].score - 1.0 / 62.0).abs() < 1e-6);
    }

    #[test]
    fn rrf_preserves_per_leg_scores_when_present() {
        // Build a vector-leg hit and a trigram-leg hit for two different
        // thoughts. After fusion, each hit should carry the per-leg score
        // from its origin (and None for the other leg).
        let v_hit = Hit::from_vector_leg(thought(1, "vec only", 0), 0.85);
        let t_hit = Hit::from_trigram_leg(thought(2, "tri only", 0), 0.42);
        let out = rrf_fuse(vec![vec![v_hit], vec![t_hit]], 60.0);
        let by_content: std::collections::HashMap<String, Hit> = out
            .into_iter()
            .map(|h| (h.thought.content.clone(), h))
            .collect();
        let v = &by_content["vec only"];
        assert_eq!(v.vector_score, Some(0.85));
        assert_eq!(v.trigram_score, None);
        let t = &by_content["tri only"];
        assert_eq!(t.vector_score, None);
        assert_eq!(t.trigram_score, Some(0.42));
    }

    #[test]
    fn rrf_merges_per_leg_scores_when_both_legs_match() {
        // Same thought appears in both legs — vector_score AND trigram_score
        // should survive the fusion.
        let v_hit = Hit::from_vector_leg(thought(1, "both", 0), 0.91);
        let t_hit = Hit::from_trigram_leg(thought(1, "both", 0), 0.33);
        let out = rrf_fuse(vec![vec![v_hit], vec![t_hit]], 60.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].vector_score, Some(0.91));
        assert_eq!(out[0].trigram_score, Some(0.33));
    }

    #[test]
    fn rrf_disjoint_rankings_union_both_with_correct_scores() {
        let r1 = vec![hit(thought(1, "a", 0), 0.9)];
        let r2 = vec![hit(thought(2, "b", 0), 0.5)];
        let out = rrf_fuse(vec![r1, r2], 60.0);
        assert_eq!(out.len(), 2);
        // Each should have score 1/61.
        assert!((out[0].score - 1.0 / 61.0).abs() < 1e-6);
        assert!((out[1].score - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn recency_boost_halves_score_at_half_life() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        // Created exactly 30 days ago.
        let mut hits = vec![hit(thought(1, "old", 30 * 86_400), 1.0)];
        recency_boost(&mut hits, 30.0, now);
        assert!((hits[0].score - 0.5).abs() < 1e-5);
    }

    #[test]
    fn recency_boost_leaves_fresh_hits_unchanged() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let mut hits = vec![hit(thought(1, "fresh", 0), 1.0)];
        recency_boost(&mut hits, 30.0, now);
        assert!((hits[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn recency_boost_resorts_when_older_hit_had_higher_raw_score() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        // Old hit with high score, fresh hit with lower score.
        // After boost the old one's score halves and the fresh one wins.
        let mut hits = vec![
            hit(thought(1, "old", 30 * 86_400), 0.8),
            hit(thought(2, "fresh", 0), 0.5),
        ];
        recency_boost(&mut hits, 30.0, now);
        assert_eq!(hits[0].thought.content, "fresh");
        assert_eq!(hits[1].thought.content, "old");
    }

    #[test]
    fn recency_boost_disabled_when_half_life_zero() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let mut hits = vec![hit(thought(1, "old", 30 * 86_400), 1.0)];
        recency_boost(&mut hits, 0.0, now);
        assert_eq!(hits[0].score, 1.0);
    }
}
