//! Change capture on the source, behind one `Tracker` trait with three backends: `compare` (the
//! always-available fallback that re-reads and hashes), `ebpf` (the dirty map via aya), and
//! `lvm_mirror` (synchronous raid1 to a network leg). The mode is chosen at preflight (§A5).
//!
//! Skeleton: the trait and the compare backend come first (the foundation path), then the eBPF
//! and mirror backends (plan.md Phases 6, 9).
