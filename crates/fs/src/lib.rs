//! Filesystem adapters for Kitten. Every external tool runs through one command runner.

pub mod btrfs;
pub mod ext4;
pub mod kernel;
pub mod mount;
pub mod ntfs;
mod parse;
pub mod probe;
pub mod run;
pub mod smart;
pub mod swap;
pub mod xfs;
