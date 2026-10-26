//! The source side: probe and preflight, the used-ranges reader, the pass loop driven by the
//! convergence model, and quiesce (hooks, swapoff, FIFREEZE, the drain check). The source's data
//! disks are read **read-only** through `bootshift-disk` (invariant M1).
//!
//! This crate ships the compare pass (§A5.2) — the always-available capture backend that reads and
//! hashes the source and sends only the chunks that differ from the receiver's index. The eBPF and
//! mirror backends and the quiesce path land in plan.md Phases 6, 7.

pub mod compare;
pub mod passes;

pub use compare::{SourceChunk, compare_pass};
pub use passes::{Outcome, SendError, Transfer, Transferred, run_passes};
