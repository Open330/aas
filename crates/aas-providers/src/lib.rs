//! Provider adapters (claude/codex/grok/zai/cursor): credential storage, auth/switch/refresh,
//! and structured `usage()`. Ported in **P2**. See `docs/PARITY_SPEC.md` §F.

/// The trait every provider adapter implements. Mirrors asx `ProviderAdapter` (base.ts).
/// Method bodies land in P2; the shape is fixed here so the CLI can be written against it.
pub trait ProviderAdapter {
    /// Canonical provider id (`claude` | `codex` | `grok` | `zai` | `cursor`).
    fn name(&self) -> &str;
}
