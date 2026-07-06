//! Cross-provider translating HTTP proxy (axum) + per-provider wire adapters + injection +
//! model registry. Ported in **P4** — the highest-risk phase. See `docs/PARITY_SPEC.md` §H
//! for the exhaustive streaming/retry/translation contract reproduced here.
//!
//! ```text
//!   agent wire ─[agent.parse_request]─▶ COMMON ─[backend.build_request]─▶ upstream
//!   upstream ─[backend.parse_stream_chunk]─▶ COMMON ─[agent.format_stream_chunk]─▶ agent wire
//! ```

pub mod adapters;
pub mod inject;
pub mod models;
pub mod retry;
pub mod server;
pub mod sse;
pub mod types;

// --- Public API (the CLI is written against this — keep it stable) ---

pub use inject::inject_proxy_endpoint;
pub use models::{backend_choices, BackendChoice};
pub use server::{start_proxy, Credential, ProxyHandle, ProxyStartOptions};

#[cfg(test)]
mod tests;
