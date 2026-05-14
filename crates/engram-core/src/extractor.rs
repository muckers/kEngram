//! The `Extractor` trait — the seam between engram and whatever backend
//! turns a thought into structured facts. Implementations live in
//! `engram-extract`.
//!
//! Mirrors the `Embedder` trait in shape and `is_transient()` discipline.
//! The reflector loop uses `is_transient()` to decide whether to soft-fail
//! and retry on the next tick (per the M2 open-question answer #9).

use async_trait::async_trait;

use crate::{Scope, Thought};

/// A single fact extracted from a thought. `subject`, `predicate`, `object`
/// are optional because not every useful fact decomposes into a clean triple;
/// the natural-language `statement` is the canonical form. `confidence` is
/// the extractor's self-reported [0.0, 1.0] score, used by the reflector to
/// route into the review queue or commit directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub statement: String,
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub confidence: f32,
}

/// Per-thought extraction mode, plumbed from the source thought's
/// `metadata.extract` field through the reflector to the extractor.
///
/// - `All` (default, also matches absent flag for back-compat): extract every
///   durable claim per the bundled prompt's rules.
/// - `DurableOnly`: the captured thought is known to mix durable claims with
///   transient session narrative; the extractor receives an additional system
///   message instructing it to extract only the durable claims. The bundled
///   prompt's mixed-content rule covers this in principle, but the operator
///   may want to lean harder per-thought.
///
/// `metadata.extract: "none"` is handled in the reflector before the
/// extractor is called and therefore does not surface here as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractMode {
    #[default]
    All,
    DurableOnly,
}

/// Knobs the reflector hands to each per-thought extraction call. `scope` is
/// echoed so the extractor can prompt-condition on it; `max_facts` caps the
/// extractor's output to keep response sizes bounded; `extract_mode` lets
/// the operator mark mixed-content thoughts at capture time (see [`ExtractMode`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionContext {
    pub scope: Scope,
    pub max_facts: usize,
    pub extract_mode: ExtractMode,
}

impl ExtractionContext {
    pub fn new(scope: Scope, max_facts: usize) -> Self {
        Self {
            scope,
            max_facts,
            extract_mode: ExtractMode::default(),
        }
    }

    /// Builder-style override for `extract_mode`. Lets the reflector compose
    /// a context that propagates a thought's `metadata.extract` decision
    /// without breaking existing callers that don't care about the mode.
    pub fn with_extract_mode(mut self, mode: ExtractMode) -> Self {
        self.extract_mode = mode;
        self
    }
}

#[async_trait]
pub trait Extractor: Send + Sync {
    /// Stable model identifier — written into `facts.extractor_model` for
    /// provenance. Conventionally `"<vendor>/<model>"`, e.g.
    /// `"openai/gpt-4o-mini"` or `"vllm/qwen2.5-7b-instruct"`.
    fn model_id(&self) -> &str;

    /// Schema-version of *this* extractor's prompt/response contract. Bump
    /// when the JSON Schema or system prompt changes in a way that makes
    /// prior facts no longer comparable. The reflector writes this into
    /// `facts.extractor_version`.
    fn version(&self) -> i32;

    /// Extract zero or more facts from a single thought. Returning an empty
    /// vec is a valid "no facts here" answer and is not a failure.
    async fn extract(
        &self,
        thought: &Thought,
        ctx: &ExtractionContext,
    ) -> Result<Vec<ExtractedFact>, ExtractorError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractorError {
    #[error("extractor timed out after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("extractor backend unreachable: {0}")]
    Unreachable(String),

    #[error("extractor returned malformed response: {0}")]
    MalformedResponse(String),

    #[error("extractor backend reported error (status {status}): {message}")]
    Backend { status: u16, message: String },

    #[error("extractor is misconfigured: {0}")]
    Misconfigured(String),
}

impl ExtractorError {
    /// True when the failure is something the next reflector tick might
    /// resolve on its own (network blip, timeout, transient 5xx). The
    /// reflector soft-fails per thought on transient errors and continues;
    /// on non-transient errors it logs and continues too — the unfacted
    /// thought remains in the LEFT-JOIN-IS-NULL set for the next tick.
    /// Either way, no row in `facts` gets written; idempotency is preserved.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::Unreachable(_) | Self::Backend { status: 500..=599, .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_transient() {
        assert!(ExtractorError::Timeout { seconds: 5 }.is_transient());
    }

    #[test]
    fn unreachable_is_transient() {
        assert!(ExtractorError::Unreachable("connection refused".into()).is_transient());
    }

    #[test]
    fn server_5xx_is_transient() {
        assert!(
            ExtractorError::Backend {
                status: 503,
                message: "unavailable".into(),
            }
            .is_transient()
        );
    }

    #[test]
    fn client_4xx_is_not_transient() {
        assert!(
            !ExtractorError::Backend {
                status: 400,
                message: "bad request".into(),
            }
            .is_transient()
        );
    }

    #[test]
    fn malformed_is_not_transient() {
        assert!(!ExtractorError::MalformedResponse("nope".into()).is_transient());
    }

    #[test]
    fn misconfigured_is_not_transient() {
        assert!(!ExtractorError::Misconfigured("missing api key".into()).is_transient());
    }

    #[test]
    fn extraction_context_round_trip() {
        let ctx = ExtractionContext::new(Scope::new("work").unwrap(), 8);
        assert_eq!(ctx.scope.as_str(), "work");
        assert_eq!(ctx.max_facts, 8);
    }

    #[test]
    fn extracted_fact_fields_are_optional_triple() {
        let f = ExtractedFact {
            statement: "Engram uses pgvector".to_string(),
            subject: Some("Engram".to_string()),
            predicate: Some("uses".to_string()),
            object: Some("pgvector".to_string()),
            confidence: 0.92,
        };
        assert!(f.subject.is_some());
        assert!((f.confidence - 0.92).abs() < f32::EPSILON);

        let g = ExtractedFact {
            statement: "Capture is immutable".to_string(),
            subject: None,
            predicate: None,
            object: None,
            confidence: 0.6,
        };
        assert!(g.subject.is_none());
    }
}
