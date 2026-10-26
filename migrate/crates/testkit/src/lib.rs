//! Cross-cutting simulation primitives shared by the migrate crates' tests: [`LiveDisk`], a disk
//! a seeded writer keeps changing while recording a ground-truth log, so the end-to-end simulation
//! can feed a moving source into the migration and check the target matches the source at the
//! freeze point. (plan.md tasks 2.1, later 2.4.)
//!
//! This is normal library code, like `bootshift-disk`'s `MemDisk`/`CrashDisk`, so any crate can
//! use it as a dev-dependency.

pub mod live_disk;

pub use live_disk::LiveDisk;
