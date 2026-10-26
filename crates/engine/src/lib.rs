//! The BootShift engine: preflight checks, the chunked copier, the eight-step executor
//! and final verification.

pub mod copier;
pub mod executor;
mod ntfs_boot;
pub mod run_plan;
