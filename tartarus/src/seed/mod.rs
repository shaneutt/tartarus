//! Per-session NoCloud seed authoring.
//!
//! Renders `user-data` + `meta-data` and authors a `cloud-init.iso`
//! carrying per-session credentials, repos, and user identity.
//! Regenerated on every `tartarus run` (no cross-invocation caching).

pub mod input;
pub mod iso;
pub mod render;
