//! The newtypes shared across the migrator. Byte offsets and lengths are plain `u64`, but the
//! identity values (epoch, frame sequence) are their own types so they can't be mixed up.

use serde::{Deserialize, Serialize};

/// One tracking block: the granularity of the dirty map, 4 KiB (the logical write size the
/// `O_DIRECT` pipeline aligns to).
pub const TRACK_BLOCK: u64 = 4096;

/// The largest range sent as a single chunk or drained as a single extent (4 MiB).
pub const MAX_EXTENT: u64 = 4 << 20;

/// A random value fixed at pairing. It is part of every frame, journal record and export name, so
/// a frame or a disk from a previous run can never be mistaken for the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Epoch(pub u64);

/// Frames are numbered from 1; the receiver acknowledges the highest one it has made durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FrameSeq(pub u64);

impl FrameSeq {
    pub fn next(self) -> Self {
        FrameSeq(self.0.saturating_add(1))
    }
}

/// A byte range `[start, end)` on a disk, always aligned to [`TRACK_BLOCK`] once it enters a
/// [`crate::DirtySet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DiskRange {
    pub start: u64,
    pub end: u64,
}

impl DiskRange {
    pub fn new(start: u64, end: u64) -> Self {
        DiskRange { start, end }
    }

    pub fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Whether the two ranges share any byte.
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}
