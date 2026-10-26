//! Filesystem adapters for BootShift. Every external tool runs through one command runner.

pub mod ext4;
pub mod ntfs;
mod parse;
pub mod run;
