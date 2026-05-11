//! engram-mcp: rmcp tool descriptors and orchestration logic for engram's
//! MCP surface.
//!
//! The orchestration functions (`capture`, plus `search_thoughts`,
//! `recent_thoughts`, `get_thought` in Phase C) are testable Rust functions
//! that take `&PgPool` + `&dyn Embedder` + a request struct. The rmcp tool
//! wiring (Phase B.2) is a thin layer over them.

pub mod capture;

pub use capture::{capture, CaptureError, CaptureRequest, CaptureResponse, MAX_CONTENT_LEN};
