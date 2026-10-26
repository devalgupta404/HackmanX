//! Block device I/O for BootShift: the `BlockDevice` trait, real devices, `MemDisk`,
//! `CrashDisk`, GPT reading and writing, and the journal store.
//! This is the only crate allowed to use `unsafe`.

mod crash_disk;
mod device;
mod mem_disk;

pub use crash_disk::{CrashDisk, PendingWrites};
pub use device::{BlockDevice, DiskError};
pub use mem_disk::MemDisk;
