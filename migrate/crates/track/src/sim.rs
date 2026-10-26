//! [`SimTracker`]: a deterministic tracker driven by the writes a `LiveDisk` records, with the
//! faults a real tracker suffers injected on demand — a silently missed write (the dangerous gap
//! the final hash compare must catch), and a hard break. It models the generation handshake of
//! the eBPF tracker (A5.1) without a kernel, so the pass loop and the convergence model can be
//! tested end to end.

use std::collections::BTreeMap;

use bootshift_migrate_core::{DirtySet, DiskRange};

use crate::tracker::{Broken, Drained, Generation, TrackError, Tracker};

pub struct SimTracker {
    current: Generation,
    /// Dirty ranges recorded per generation, kept until drained.
    buffers: BTreeMap<Generation, DirtySet>,
    broken: Option<Broken>,
    miss_next: usize,
}

impl SimTracker {
    pub fn new() -> Self {
        let mut buffers = BTreeMap::new();
        buffers.insert(Generation(0), DirtySet::new());
        SimTracker {
            current: Generation(0),
            buffers,
            broken: None,
            miss_next: 0,
        }
    }

    /// A write the source issued, recorded in the current generation — unless a miss is scheduled,
    /// in which case it is silently dropped, the case `Drained` can't reveal and only the final
    /// hash compare catches (invariant M4's reason to never trust the tracker alone).
    pub fn record_write(&mut self, range: DiskRange) {
        if self.miss_next > 0 {
            self.miss_next -= 1;
            return;
        }
        self.buffers.entry(self.current).or_default().mark(range);
    }

    /// Silently miss the next `count` writes (a tracking gap).
    pub fn miss_next_writes(&mut self, count: usize) {
        self.miss_next += count;
    }

    /// Mark tracking broken from now on; every drain reports it (M4).
    pub fn break_tracking(&mut self, reason: Broken) {
        self.broken = Some(reason);
    }
}

impl Default for SimTracker {
    fn default() -> Self {
        SimTracker::new()
    }
}

impl Tracker for SimTracker {
    fn confirm_live(&mut self) -> Result<(), TrackError> {
        Ok(())
    }

    fn flip(&mut self) -> Result<Generation, TrackError> {
        let old = self.current;
        self.current = self.current.next();
        self.buffers.entry(self.current).or_default();
        Ok(old)
    }

    fn drain(&mut self, generation: Generation) -> Result<Drained, TrackError> {
        if let Some(reason) = self.broken {
            return Ok(Drained::Broken(reason));
        }
        // Only a generation the sender has already flipped past is safe to drain.
        if generation >= self.current {
            return Err(TrackError::WrongGeneration {
                asked: generation,
                current: self.current,
            });
        }
        let set = self.buffers.remove(&generation).unwrap_or_default();
        Ok(Drained::Ranges(set.ranges().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLK: u64 = 4096;

    fn r(start_blocks: u64, end_blocks: u64) -> DiskRange {
        DiskRange::new(start_blocks * BLK, end_blocks * BLK)
    }

    fn drained_ranges(drained: Drained) -> Vec<DiskRange> {
        match drained {
            Drained::Ranges(ranges) => ranges,
            Drained::Broken(reason) => panic!("unexpected broken: {reason:?}"),
        }
    }

    #[test]
    fn a_drained_generation_covers_every_write_it_saw() {
        let mut tracker = SimTracker::new();
        tracker.record_write(r(0, 1));
        tracker.record_write(r(5, 6));
        tracker.record_write(r(1, 2)); // merges with the first
        let generation = tracker.flip().expect("flip");
        let ranges = drained_ranges(tracker.drain(generation).expect("drain"));
        assert_eq!(ranges, vec![r(0, 2), r(5, 6)]);
    }

    #[test]
    fn writes_after_a_flip_land_in_the_next_generation() {
        let mut tracker = SimTracker::new();
        tracker.record_write(r(0, 1));
        let gen0 = tracker.flip().expect("flip");
        tracker.record_write(r(9, 10)); // after the flip
        let ranges0 = drained_ranges(tracker.drain(gen0).expect("drain gen0"));
        assert_eq!(
            ranges0,
            vec![r(0, 1)],
            "the later write is not in the old generation"
        );
        let gen1 = tracker.flip().expect("flip");
        let ranges1 = drained_ranges(tracker.drain(gen1).expect("drain gen1"));
        assert_eq!(ranges1, vec![r(9, 10)]);
    }

    #[test]
    fn a_missed_write_is_absent_from_the_drain() {
        let mut tracker = SimTracker::new();
        tracker.miss_next_writes(1);
        tracker.record_write(r(0, 1)); // silently missed
        tracker.record_write(r(2, 3));
        let generation = tracker.flip().expect("flip");
        let ranges = drained_ranges(tracker.drain(generation).expect("drain"));
        assert_eq!(
            ranges,
            vec![r(2, 3)],
            "the missed write is a gap the compare pass must catch"
        );
    }

    #[test]
    fn broken_tracking_is_reported_on_drain() {
        let mut tracker = SimTracker::new();
        tracker.record_write(r(0, 1));
        tracker.break_tracking(Broken::Detached);
        let generation = tracker.flip().expect("flip");
        assert_eq!(
            tracker.drain(generation),
            Ok(Drained::Broken(Broken::Detached))
        );
    }

    #[test]
    fn draining_the_live_generation_is_an_error() {
        let mut tracker = SimTracker::new();
        let generation = tracker.flip().expect("flip"); // generation 0 drained-able, current is 1
        tracker
            .drain(generation)
            .expect("generation 0 is drainable");
        let live = tracker.current;
        assert_eq!(
            tracker.drain(live),
            Err(TrackError::WrongGeneration {
                asked: Generation(1),
                current: Generation(1),
            })
        );
    }
}
