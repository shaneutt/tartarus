//! Provider-agnostic session-shape types: error variants, UUID +
//! alias resolution, run-mode selection. Provider implementations
//! consume these and the binary surfaces them to the CLI.

pub mod error;
pub mod identity;
pub mod metadata;
pub mod run_mode;

pub use error::SessionError;
pub use identity::{ResolvedSession, is_valid_alias, is_valid_uuid, new_uuid};
pub use metadata::{GpuBorrowRecord, Metadata};
pub use run_mode::RunMode;
