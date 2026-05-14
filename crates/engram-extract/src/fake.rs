//! `FakeExtractor` — deterministic, in-memory `Extractor` for tests.
//!
//! Given the same thought, emits the same facts — useful for asserting
//! exact row contents in `sqlx::test`-driven reflector tests. Configurable
//! to always fail in specific ways for testing the soft-fail path.
//!
//! Mirrors `engram-embed::FakeEmbedder` in shape.

use async_trait::async_trait;
use engram_core::{ExtractedFact, ExtractionContext, Extractor, ExtractorError, Thought};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct FakeExtractor {
    model_id: String,
    version: i32,
    behavior: FakeBehavior,
    /// When `Some`, the deterministic mode returns exactly these facts
    /// regardless of input. When `None`, it emits a single fact echoing
    /// the thought content at `default_confidence`.
    output_override: Option<Vec<ExtractedFact>>,
    default_confidence: f32,
    /// Records the `ExtractionContext` of the most recent successful
    /// `extract()` call, so tests can assert that the reflector propagated
    /// `metadata.extract` / `ExtractMode` correctly. `Arc<Mutex<_>>` because
    /// `FakeExtractor` is passed by shared reference through the reflector
    /// loop and tests need to inspect the post-run state.
    last_ctx: Arc<Mutex<Option<ExtractionContext>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeBehavior {
    /// Return a deterministic fact derived from the thought.
    Deterministic,
    /// Always fail with `ExtractorError::Timeout`.
    Timeout,
    /// Always fail with `ExtractorError::Unreachable`.
    Unreachable,
    /// Always fail with `ExtractorError::Misconfigured`.
    Misconfigured,
}

impl FakeExtractor {
    /// New deterministic extractor with sensible defaults (model_id
    /// `"fake/extractor"`, version 1, confidence 0.9 per fact).
    pub fn new() -> Self {
        Self {
            model_id: "fake/extractor".to_string(),
            version: 1,
            behavior: FakeBehavior::Deterministic,
            output_override: None,
            default_confidence: 0.9,
            last_ctx: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the `ExtractionContext` of the most recent successful
    /// `extract()` call, if any. Tests use this to assert the reflector
    /// propagated `metadata.extract` → `ExtractMode` correctly.
    pub fn last_ctx(&self) -> Option<ExtractionContext> {
        self.last_ctx.lock().expect("last_ctx mutex poisoned").clone()
    }

    pub fn with_model(model_id: impl Into<String>, version: i32) -> Self {
        Self {
            model_id: model_id.into(),
            version,
            ..Self::new()
        }
    }

    /// Build a deterministic extractor that emits its single fact with the
    /// given confidence. Drives the reflector's review-queue routing tests.
    pub fn with_confidence(confidence: f32) -> Self {
        Self {
            default_confidence: confidence,
            ..Self::new()
        }
    }

    /// Build a deterministic extractor that always returns exactly the
    /// given facts. Drives tests that need explicit subject/predicate/object
    /// values or multiple facts per thought.
    pub fn with_facts(facts: Vec<ExtractedFact>) -> Self {
        Self {
            output_override: Some(facts),
            ..Self::new()
        }
    }

    /// Build a copy of this extractor that always fails with the given
    /// behavior. Drives the soft-fail path in the reflector tests.
    pub fn always_failing(behavior: FakeBehavior) -> Self {
        Self {
            behavior,
            ..Self::new()
        }
    }
}

impl Default for FakeExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Extractor for FakeExtractor {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn version(&self) -> i32 {
        self.version
    }

    async fn extract(
        &self,
        thought: &Thought,
        ctx: &ExtractionContext,
    ) -> Result<Vec<ExtractedFact>, ExtractorError> {
        match self.behavior {
            FakeBehavior::Timeout => Err(ExtractorError::Timeout { seconds: 5 }),
            FakeBehavior::Unreachable => Err(ExtractorError::Unreachable(
                "fake extractor configured to fail".into(),
            )),
            FakeBehavior::Misconfigured => Err(ExtractorError::Misconfigured(
                "fake extractor configured to fail".into(),
            )),
            FakeBehavior::Deterministic => {
                *self.last_ctx.lock().expect("last_ctx mutex poisoned") = Some(ctx.clone());
                let facts = if let Some(ref overridden) = self.output_override {
                    overridden.clone()
                } else {
                    vec![ExtractedFact {
                        statement: thought.content.clone(),
                        subject: None,
                        predicate: None,
                        object: None,
                        confidence: self.default_confidence,
                    }]
                };
                Ok(facts.into_iter().take(ctx.max_facts).collect())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::{Metadata, Scope, Source, ThoughtId};
    use time::OffsetDateTime;

    fn make_thought(content: &str) -> Thought {
        Thought {
            id: ThoughtId::new(),
            scope: Scope::global(),
            content: content.to_string(),
            source: Source::new("test").unwrap(),
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            metadata: Metadata::empty(),
        }
    }

    fn ctx(max_facts: usize) -> ExtractionContext {
        ExtractionContext::new(Scope::global(), max_facts)
    }

    #[tokio::test]
    async fn extracts_deterministic_fact_from_thought() {
        let e = FakeExtractor::new();
        let t = make_thought("Engram uses pgvector");
        let facts = e.extract(&t, &ctx(8)).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].statement, "Engram uses pgvector");
        assert!((facts[0].confidence - 0.9).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn respects_with_confidence() {
        let e = FakeExtractor::with_confidence(0.42);
        let facts = e.extract(&make_thought("x"), &ctx(8)).await.unwrap();
        assert!((facts[0].confidence - 0.42).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn respects_with_facts_override() {
        let override_facts = vec![
            ExtractedFact {
                statement: "first".into(),
                subject: Some("S".into()),
                predicate: Some("P".into()),
                object: Some("O".into()),
                confidence: 0.95,
            },
            ExtractedFact {
                statement: "second".into(),
                subject: None,
                predicate: None,
                object: None,
                confidence: 0.55,
            },
        ];
        let e = FakeExtractor::with_facts(override_facts.clone());
        let facts = e.extract(&make_thought("ignored"), &ctx(10)).await.unwrap();
        assert_eq!(facts, override_facts);
    }

    #[tokio::test]
    async fn caps_at_max_facts() {
        let many = (0..5)
            .map(|i| ExtractedFact {
                statement: format!("fact {i}"),
                subject: None,
                predicate: None,
                object: None,
                confidence: 0.9,
            })
            .collect();
        let e = FakeExtractor::with_facts(many);
        let facts = e.extract(&make_thought("x"), &ctx(3)).await.unwrap();
        assert_eq!(facts.len(), 3);
    }

    #[tokio::test]
    async fn always_failing_timeout_returns_timeout() {
        let e = FakeExtractor::always_failing(FakeBehavior::Timeout);
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        assert!(matches!(err, ExtractorError::Timeout { .. }));
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn always_failing_unreachable_returns_unreachable() {
        let e = FakeExtractor::always_failing(FakeBehavior::Unreachable);
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        assert!(matches!(err, ExtractorError::Unreachable(_)));
        assert!(err.is_transient());
    }

    #[tokio::test]
    async fn always_failing_misconfigured_is_not_transient() {
        let e = FakeExtractor::always_failing(FakeBehavior::Misconfigured);
        let err = e.extract(&make_thought("x"), &ctx(8)).await.unwrap_err();
        assert!(matches!(err, ExtractorError::Misconfigured(_)));
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn model_id_and_version_are_stable() {
        let e = FakeExtractor::with_model("custom/m", 7);
        assert_eq!(e.model_id(), "custom/m");
        assert_eq!(e.version(), 7);
    }
}
