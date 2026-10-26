//! Block device I/O for BootShift: the `BlockDevice` trait, real devices, `MemDisk`,
//! `CrashDisk`, GPT reading and writing, and the journal store.
//! This is the only crate allowed to use `unsafe`.
