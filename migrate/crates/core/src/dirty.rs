//! The dirty-range set: the ranges of a disk that may have changed since they were last read.
//! Kept sorted and merged so a burst of small writes collapses to a few extents, and drained as
//! pieces no larger than [`MAX_EXTENT`] for the reader and the wire.
//!
//! Every range is snapped out to whole [`TRACK_BLOCK`]s before it is stored: the tracker works at
//! block granularity, and a range that covers part of a block must cover all of it.

use crate::units::{DiskRange, TRACK_BLOCK};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirtySet {
    /// Sorted by start, never overlapping or touching (touching ranges are merged).
    ranges: Vec<DiskRange>,
}

impl DirtySet {
    pub fn new() -> Self {
        DirtySet::default()
    }

    /// The set covering exactly the given ranges (for a bulk pass: the used ranges of the disk).
    pub fn from_ranges(ranges: impl IntoIterator<Item = DiskRange>) -> Self {
        let mut set = DirtySet::new();
        set.extend(ranges);
        set
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| r.len()).sum()
    }

    pub fn ranges(&self) -> &[DiskRange] {
        &self.ranges
    }

    /// Marks a range dirty, snapped out to whole tracking blocks and merged with its neighbours.
    pub fn mark(&mut self, range: DiskRange) {
        let snapped = DiskRange::new(align_down(range.start), align_up(range.end));
        if snapped.is_empty() {
            return;
        }
        let at = self.ranges.partition_point(|r| r.start < snapped.start);
        self.ranges.insert(at, snapped);
        self.merge_around(at);
    }

    /// Marks the whole disk dirty. This is M4's fail-closed path: any gap in tracking makes the
    /// next pass a full compare.
    pub fn mark_all(&mut self, disk_len: u64) {
        self.ranges = if disk_len == 0 {
            Vec::new()
        } else {
            vec![DiskRange::new(0, align_up(disk_len))]
        };
    }

    pub fn extend(&mut self, ranges: impl IntoIterator<Item = DiskRange>) {
        for range in ranges {
            self.mark(range);
        }
    }

    /// Drains the whole set into extents no larger than `max_extent` (rounded up to a tracking
    /// block). The set is left empty.
    pub fn take_extents(&mut self, max_extent: u64) -> Vec<DiskRange> {
        let max = align_up(max_extent.max(TRACK_BLOCK));
        let mut out = Vec::new();
        for range in self.ranges.drain(..) {
            let mut start = range.start;
            while start < range.end {
                let end = start.saturating_add(max).min(range.end);
                out.push(DiskRange::new(start, end));
                start = end;
            }
        }
        out
    }

    /// Merges the range just inserted at `at` with any neighbour it now touches or overlaps.
    fn merge_around(&mut self, at: usize) {
        // Merge forward.
        while at + 1 < self.ranges.len() && self.ranges[at + 1].start <= self.ranges[at].end {
            let next = self.ranges.remove(at + 1);
            self.ranges[at].end = self.ranges[at].end.max(next.end);
        }
        // Merge backward.
        let mut here = at;
        while here > 0 && self.ranges[here].start <= self.ranges[here - 1].end {
            let this = self.ranges.remove(here);
            self.ranges[here - 1].end = self.ranges[here - 1].end.max(this.end);
            here -= 1;
        }
    }
}

fn align_down(value: u64) -> u64 {
    value - value % TRACK_BLOCK
}

fn align_up(value: u64) -> u64 {
    value.div_ceil(TRACK_BLOCK).saturating_mul(TRACK_BLOCK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const BLK: u64 = TRACK_BLOCK;

    #[test]
    fn marking_merges_touching_and_overlapping_ranges() {
        let mut set = DirtySet::new();
        set.mark(DiskRange::new(0, BLK));
        set.mark(DiskRange::new(BLK, 2 * BLK)); // touches the first
        set.mark(DiskRange::new(10 * BLK, 11 * BLK)); // separate
        set.mark(DiskRange::new(BLK, 3 * BLK)); // overlaps the merged first
        assert_eq!(
            set.ranges(),
            &[
                DiskRange::new(0, 3 * BLK),
                DiskRange::new(10 * BLK, 11 * BLK)
            ]
        );
        assert_eq!(set.total_bytes(), 4 * BLK);
    }

    #[test]
    fn a_sub_block_write_marks_the_whole_block() {
        let mut set = DirtySet::new();
        set.mark(DiskRange::new(10, 20));
        assert_eq!(set.ranges(), &[DiskRange::new(0, BLK)]);
    }

    #[test]
    fn take_extents_splits_to_the_maximum_and_empties_the_set() {
        let mut set = DirtySet::new();
        set.mark(DiskRange::new(0, 10 * BLK));
        let extents = set.take_extents(4 * BLK);
        assert_eq!(
            extents,
            vec![
                DiskRange::new(0, 4 * BLK),
                DiskRange::new(4 * BLK, 8 * BLK),
                DiskRange::new(8 * BLK, 10 * BLK),
            ]
        );
        assert!(set.is_empty());
    }

    #[test]
    fn mark_all_covers_the_rounded_up_disk() {
        let mut set = DirtySet::new();
        set.mark_all(BLK + 1);
        assert_eq!(set.ranges(), &[DiskRange::new(0, 2 * BLK)]);
    }

    proptest! {
        /// However ranges are marked, the stored set is sorted, non-overlapping, non-touching,
        /// block-aligned, and draining it yields back exactly those bytes.
        #[test]
        fn the_set_is_always_sorted_merged_and_covers_what_was_marked(
            marks in prop::collection::vec((0u64..2000, 0u64..2000), 0..40)
        ) {
            let mut set = DirtySet::new();
            let mut covered = std::collections::BTreeSet::new();
            for (a, b) in marks {
                let (lo, hi) = (a.min(b), a.max(b));
                set.mark(DiskRange::new(lo, hi));
                for block in (align_down(lo) / BLK)..(align_up(hi) / BLK) {
                    if hi > lo {
                        covered.insert(block);
                    }
                }
            }
            // Sorted, merged, block-aligned.
            for pair in set.ranges().windows(2) {
                prop_assert!(pair[0].end < pair[1].start, "touching ranges were not merged");
            }
            for range in set.ranges() {
                prop_assert_eq!(range.start % BLK, 0);
                prop_assert_eq!(range.end % BLK, 0);
                prop_assert!(!range.is_empty());
            }
            // Draining covers exactly the marked blocks.
            let drained = set.take_extents(3 * BLK);
            let mut from_drain = std::collections::BTreeSet::new();
            for range in &drained {
                prop_assert!(range.len() <= 3 * BLK);
                for block in (range.start / BLK)..(range.end / BLK) {
                    from_drain.insert(block);
                }
            }
            prop_assert_eq!(from_drain, covered);
            prop_assert!(set.is_empty());
        }
    }
}
