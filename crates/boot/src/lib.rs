//! Boot handling for Kitten: ESP files, the GRUB config redirect, EFI boot order
//! and the recovery initramfs builder.

pub mod cmdline;
pub mod cpio;
pub mod efi;
pub mod grub;
pub mod initramfs;
pub mod journal_file;
