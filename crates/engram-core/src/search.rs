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
/// fusion layer accumulates and re-orders them. `score` is the raw score
/// from the producing leg (cosine similarity, trigram similarity, etc.) for
/// pre-fusion; after fusion, it's the RRF or boosted-RRF score.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub thought: Thought,
    pub score: f32,
}

/// Reciprocal Rank Fusion. Each ranking is taken in the order given.
/// Output is sorted by descending fused score.
pub fn rrf_fuse(rankings: Vec<Vec<Hit>>, k: f32) -> Vec<Hit> {
    let mut acc: HashMap<crate::ThoughtId, (Thought, f32)> = HashMap::new();

    for ranking in rankings {
        for (i, hit) in ranking.into_iter().enumerate() {
            let rank = (i + 1) as f32;
            let contribution = 1.0 / (k + rank);
            acc.entry(hit.thought.id)
                .and_modify(|(_, s)| *s += contribution)
                .or_insert((hit.thought, contribution));
        }
    }

    let mut fused: Vec<Hit> = acc
        .into_values()
        .map(|(thought, score)| Hit { thought, score })
        .collect();

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
        Hit { thought: t, score }
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
