//! The sender pass loop (implementation I5.1): the policy that drives a migration to the point a
//! cutover is cheap. It flips the tracker before each read and drains it after (M5's overlapping
//! window), folds the drained writes and anything the receiver has not yet acknowledged back into
//! the dirty set, and asks the convergence model after every pass whether to hold ready, keep
//! going, or surface a decision. If the tracker ever reports broken (M4), the loop stops trusting
//! it and every following pass is a full hash compare, which cannot miss a silently dropped write.
//!
//! The loop owns only the policy. The I/O — reading the source read-only, hashing, framing, sending
//! over the link, and learning what the receiver has acknowledged — lives behind the [`Transfer`]
//! trait, so the policy is tested against a deterministic in-memory transfer with no kernel, no
//! threads and no network.

use std::time::Duration;

use bootshift_disk::DiskError;
use bootshift_migrate_core::{
    Advice, ConvergenceModel, DirtySet, DiskRange, MAX_EXTENT, PassKind, PassStats,
};
use bootshift_migrate_track::{Drained, TrackError, Tracker};

/// What a single transfer step moved, as the sink measured it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transferred {
    pub bytes: u64,
    /// Effective rate over the step: `min(link, read, apply)` in bytes per second.
    pub rate: u64,
    pub duration: Duration,
}

/// The source side's I/O, behind a trait so the pass loop can be driven without a kernel or a
/// network. Every method reads the source **read-only** (invariant M1).
pub trait Transfer {
    /// Read the given source ranges, hash and send them, and apply them at the target. Returns what
    /// was moved.
    fn send_ranges(&mut self, ranges: &[DiskRange]) -> Result<Transferred, DiskError>;

    /// Compare the whole source against the receiver's index and send only the chunks that differ
    /// (A5.2). The recovery path when tracking breaks, and the exact final delta after a freeze.
    fn compare_all(&mut self) -> Result<Transferred, DiskError>;

    /// Ranges the sender sent but the receiver has not yet durably acknowledged, to resend next
    /// pass (U2: resume, never restart). Empty once everything sent is acknowledged.
    fn unacknowledged(&mut self) -> Vec<DiskRange>;
}

/// Where the loop left off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A cutover now would cost no more than the budget. Freeze, run one final pass, then commit.
    Ready { predicted_downtime: Duration },
    /// The model cannot get under budget on its own and needs a human or a mode change.
    NeedsDecision(Advice),
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("tracker error: {0}")]
    Track(#[from] TrackError),
    #[error("disk error: {0}")]
    Disk(#[from] DiskError),
    #[error("did not converge within {passes} passes")]
    NoConvergence { passes: usize },
}

/// Drive passes until the convergence model reaches a verdict (I5.1). `used` is the source's used
/// ranges, the bulk pass's starting dirty set. The loop never reads or writes a real device itself;
/// all I/O goes through `transfer`.
pub fn run_passes(
    transfer: &mut dyn Transfer,
    tracker: &mut dyn Tracker,
    used: &[DiskRange],
    model: &mut ConvergenceModel,
    max_passes: usize,
) -> Result<Outcome, SendError> {
    tracker.confirm_live()?;

    let mut dirty = DirtySet::from_ranges(used.iter().copied());
    let mut tracking_broken = false;
    let mut kind = PassKind::Bulk;

    for _ in 0..max_passes {
        if tracking_broken {
            // M4: the tracker can no longer be trusted, so sweep the whole source by hash. What is
            // still outstanding afterwards is only what the receiver has not yet acknowledged.
            let report = transfer.compare_all()?;
            dirty = DirtySet::new();
            dirty.extend(transfer.unacknowledged());
            model.record(PassStats {
                kind: PassKind::Compare,
                sent_bytes: report.bytes,
                duration: report.duration,
                dirty_at_end: dirty.total_bytes(),
                rate: report.rate,
            });
        } else {
            let generation = tracker.flip()?;
            let extents = dirty.take_extents(MAX_EXTENT);
            let report = transfer.send_ranges(&extents)?;
            match tracker.drain(generation)? {
                Drained::Broken(reason) => {
                    // The whole disk is now suspect; defer the verdict until a full compare has run,
                    // so this pass's drained-but-untrustworthy dirty set never declares *ready*.
                    log::warn!("tracking broke ({reason:?}); every further pass is a hash compare");
                    tracking_broken = true;
                    continue;
                }
                Drained::Ranges(changed) => dirty.extend(changed),
            }
            dirty.extend(transfer.unacknowledged());
            model.record(PassStats {
                kind,
                sent_bytes: report.bytes,
                duration: report.duration,
                dirty_at_end: dirty.total_bytes(),
                rate: report.rate,
            });
            kind = PassKind::Delta;
        }

        match model.advise() {
            Advice::Ready { predicted_downtime } => {
                return Ok(Outcome::Ready { predicted_downtime });
            }
            Advice::Continue => continue,
            other => return Ok(Outcome::NeedsDecision(other)),
        }
    }

    Err(SendError::NoConvergence { passes: max_passes })
}
