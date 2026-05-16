//! engram-core: domain types, the `Embedder` trait, and retrieval fusion logic.
//!
//! Pure logic, no I/O. Implementations live in `engram-storage` (Postgres),
//! `engram-embed` (TEI/Ollama/OpenAI), and `engram-mcp` (rmcp tool handlers).

pub mod embedder;
pub mod embedding;
pub mod extractor;
pub mod fact;
pub mod metadata;
pub mod metrics;
pub mod scope;
pub mod search;
pub mod source;
pub mod thought;

pub use embedder::{Embedder, EmbedderError};
pub use embedding::{Embedding, EmbeddingError, EmbeddingModel, EmbeddingStatus};
pub use extractor::{ExtractMode, ExtractedFact, ExtractionContext, Extractor, ExtractorError};
pub use fact::Fact;
pub use metadata::Metadata;
pub use metrics::{ndcg_at_k, reciprocal_rank};
pub use scope::{Scope, ScopeError};
pub use search::{DEFAULT_RECENCY_HALF_LIFE_DAYS, DEFAULT_RRF_K, Hit, recency_boost, rrf_fuse};
pub use source::{Source, SourceError};
pub use thought::{Thought, ThoughtId};
