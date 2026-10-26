//! Change capture on the source, behind one [`Tracker`] trait with three real backends to come —
//! `compare` (re-read and hash, the always-available fallback), `ebpf` (the dirty map via aya) and
//! `lvm_mirror` (synchronous raid1 to a network leg) — chosen at preflight (§A5).
//!
//! This crate ships the trait and [`SimTracker`], the deterministic fake the end-to-end simulation
//! drives (plan.md task 2.2), with the faults a real tracker suffers: a missed write, a detach, a
//! passthrough write. The one rule a tracker must keep is **fail closed** (invariant M4): if it
//! can't prove it saw every write, it says so, and the pass becomes a full compare.

#[cfg(feature = "ebpf")]
pub mod ebpf;
pub mod sim;
pub mod tracker;

#[cfg(feature = "ebpf")]
pub use ebpf::{EbpfError, EbpfTracker};
pub use sim::SimTracker;
pub use tracker::{Broken, Drained, Generation, TrackError, Tracker};
