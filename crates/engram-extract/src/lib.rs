//! engram-extract: `Extractor` implementations.
//!
//! Phase A ships the crate skeleton only. Phase C adds:
//!
//! - `OpenAICompatibleExtractor` — talks to anything speaking the OpenAI
//!   `/v1/chat/completions` API with `response_format: json_schema` (vLLM
//!   production, OpenRouter cloud fallback). Endpoint and model name come
//!   from config.
//! - `FakeExtractor` — deterministic in-memory extractor for tests; mirrors
//!   `FakeEmbedder` in shape.
//!
//! The trait itself lives in `engram-core` (`engram_core::Extractor`) so
//! `engram-mcp` and the reflector can depend on the abstraction without
//! pulling in this crate's HTTP machinery.
