//! engram-core: domain types, the `Embedder` trait, and retrieval fusion logic.
//!
//! Pure logic, no I/O. Implementations live in `engram-storage` (Postgres),
//! `engram-embed` (TEI/Ollama/OpenAI), and `engram-mcp` (rmcp tool handlers).

pub mod embedder;
pub mod embedding;
pub mod metadata;
pub mod scope;
pub mod source;
pub mod thought;

pub use embedder::{Embedder, EmbedderError};
pub use embedding::{Embedding, EmbeddingError, EmbeddingModel, EmbeddingStatus};
pub use metadata::Metadata;
pub use scope::{Scope, ScopeError};
pub use source::{Source, SourceError};
pub use thought::{Thought, ThoughtId};
