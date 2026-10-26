//! The tracker interface: "give me the ranges that may have changed since a generation, or tell me
//! tracking broke." The generation handshake (flip before read, drain after) is invariant M5's
//! overlapping-window rule, and [`Drained::Broken`] is M4's fail-closed path.

use bootshift_migrate_core::DiskRange;

/// A tracking generation. The sender flips it before each pass's read, so every write after the
/// flip is drained in a generation the sender will take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(pub u64);

impl Generation {
    pub fn next(self) -> Self {
        Generation(self.0.saturating_add(1))
    }
}

/// Why tracking can no longer be trusted (M4). Any of these marks the whole disk dirty, so the
/// next pass is a full hash compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    /// The program detached or the agent restarted.
    Detached,
    /// A counter showed lost events, or a range too large to mark.
    LostEvents,
    /// A write reached the disk through a path the block tracker can't see (SG_IO, NVMe passthrough).
    Passthrough,
    /// The sender's heartbeat write did not show up in the drain.
    HeartbeatMissed,
}

/// The result of draining a generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drained {
    Ranges(Vec<DiskRange>),
    Broken(Broken),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrackError {
    #[error(
        "asked to drain generation {asked:?}, which is not the one being drained ({current:?})"
    )]
    WrongGeneration {
        asked: Generation,
        current: Generation,
    },
    /// A real backend (the eBPF maps) failed in a way draining can't fail closed from, such as the
    /// generation flip itself. The sim backend never raises this.
    #[error("tracker backend error: {0}")]
    Backend(String),
}

pub trait Tracker {
    /// Confirm tracking is live before the first read of a pass (M5). Reports `Broken` through the
    /// error-free `Drained` channel on the next drain; a hard failure to attach is a `TrackError`.
    fn confirm_live(&mut self) -> Result<(), TrackError>;

    /// Move new writes into the next generation and return the generation now safe to drain.
    fn flip(&mut self) -> Result<Generation, TrackError>;

    /// Take the dirty ranges recorded in `generation`, or report that tracking broke (M4).
    fn drain(&mut self, generation: Generation) -> Result<Drained, TrackError>;
}
