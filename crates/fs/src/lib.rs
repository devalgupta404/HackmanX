//! Filesystem adapters for BootShift. Every external tool runs through one command runner.

pub mod ext4;
pub mod kernel;
pub mod ntfs;
mod parse;
pub mod probe;
pub mod run;
pub mod smart;
pub mod swap;
