//! The target side of a migration: the raw scratch store (journal slots and the hash index), the
//! applier (write → flush → read-back → verify → index → journal → acknowledge, invariant M3), and
//! finalize (layout, grow, adapt, boot). Reuses `bootshift-disk`, `bootshift-fs` and
//! `bootshift-boot`.
//!
//! Built out in plan.md Phases 4 and 8. The store (§I4) is here; the applier and finalize land on
//! top of it.

pub mod store;

pub use store::{Slot, Store, StoreError};
