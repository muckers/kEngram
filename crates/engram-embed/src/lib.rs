//! engram-embed: `Embedder` implementations.
//!
//! - [`OpenAICompatibleEmbedder`] talks to anything speaking the OpenAI
//!   `/v1/embeddings` API — Ollama (dev default), TEI sidecar (production),
//!   OpenAI, Voyage. Endpoint and model name come from config.
//! - [`FakeEmbedder`] is a deterministic in-memory embedder for tests; it
//!   does not require Ollama / TEI to be running.

pub mod fake;
pub mod openai_compatible;

pub use fake::{FakeBehavior, FakeEmbedder};
pub use openai_compatible::{OpenAICompatibleConfig, OpenAICompatibleEmbedder};
