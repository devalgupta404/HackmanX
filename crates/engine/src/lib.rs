//! The BootShift engine: preflight checks, the chunked copier, the eight-step executor
//! and final verification.

pub mod copier;
pub mod executor;
pub mod facts;
mod ntfs_boot;
pub mod preflight;
pub mod run_plan;
pub mod system_filesystems;
