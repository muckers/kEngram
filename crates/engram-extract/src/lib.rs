//! engram-extract: `Extractor` implementations.
//!
//! - [`OpenAICompatibleExtractor`] talks to anything that speaks the OpenAI
//!   `/v1/chat/completions` API with `response_format: json_schema` — vLLM
//!   (production sidecar), OpenRouter (cloud fallback), OpenAI itself.
//!   Distinguished only by config.
//! - [`FakeExtractor`] is a deterministic in-memory extractor for tests;
//!   mirrors `engram-embed::FakeEmbedder` in shape.
//!
//! The trait itself lives in `engram-core` (`engram_core::Extractor`) so
//! `engram-mcp::reflect` and the reflector loop in `engram-cli` can depend
//! on the abstraction without pulling in this crate's HTTP machinery.

pub mod fake;
pub mod openai_compatible;

pub use fake::{FakeBehavior, FakeExtractor};
pub use openai_compatible::{OpenAICompatibleConfig, OpenAICompatibleExtractor};
