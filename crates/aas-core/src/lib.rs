//! `aas-core` — foundational, provider-agnostic layer ported from asx's `storage/` + `utils/`.
//!
//! Data + filesystem logic: platform paths, account store, profile homes, sharing categories,
//! JWT claim decode, keychain service-name derivation + secure credential store, and the
//! structured usage model. Network HTTP lives in `aas-providers`; the proxy in `aas-proxy`.
//! (The one subprocess here is the macOS `security` CLI, used by `secure_store`/`keychain`.)

pub mod backoff;
pub mod execargs;
pub mod jwt;
pub mod keychain;
pub mod keyed_lock;
pub mod model;
pub mod naming;
pub mod platform;
pub mod secure_store;
pub mod share;
pub mod store;
pub mod usage;
pub mod usage_cache;

pub use model::{AccountRecord, ProfileType, Store};
