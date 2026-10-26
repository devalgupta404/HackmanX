//! Boot handling for BootShift: ESP files, the GRUB config redirect, EFI boot order
//! and the recovery initramfs builder.

pub mod cmdline;
pub mod efi;
pub mod grub;
pub mod journal_file;
