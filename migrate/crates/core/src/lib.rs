//! Pure logic for BootShift Migrate: the shared units, the dirty-range set, the convergence
//! model and the fail-closed cutover state machines. No I/O and no `unsafe`, so every rule here
//! is exhaustively testable without a disk, a kernel or a network. The crates that do I/O
//! (`migrate-send`, `migrate-receive`, `migrate-track`, `migrate-net`) build on these types.
//!
//! See `migrate/plan.md`, `architecture.md` and `implementation.md` for the design these
//! implement; invariants are referenced as M1–M11.

pub mod converge;
pub mod cutover;
pub mod dirty;
pub mod layout;
pub mod proto;
pub mod receipt;
pub mod units;

pub use converge::{Advice, ConvergenceModel, PassKind, PassStats};
pub use cutover::{Fence, ReceiverState, SenderState};
pub use dirty::DirtySet;
pub use layout::{LayoutError, LayoutMap};
pub use proto::{Codec, CodecError, Message, Mode, PROTOCOL_VERSION, Role, decode, encode};
pub use receipt::{Receipt, merkle_root};
pub use units::{DiskRange, Epoch, FrameSeq, MAX_EXTENT, TRACK_BLOCK};
