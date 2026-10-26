//! The target side: the raw scratch store (journal, intent log, hash index, receipt), the
//! applier (write → flush → read-back → verify → index → journal → ack, invariant M3), and
//! finalize (layout, grow, adapt through the undo log, boot by kexec). Reuses `bootshift-disk`,
//! `bootshift-fs` and `bootshift-boot`. Built out in plan.md Phases 4, 8.
//!
//! Skeleton: lands on top of `migrate-core` and the BootShift I/O crates.
