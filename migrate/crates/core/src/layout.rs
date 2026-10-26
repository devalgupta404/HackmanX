//! The target layout map (plan.md task 3.6): where each source range lands on the (often bigger)
//! target disk. It is a list of equal-length `(source, target)` segments — a partition may move to
//! a new offset, but a byte is only ever moved, never changed, so each segment keeps its length.
//! Filesystems are grown into the new space *after* the copy, not here (invariant M9: identity is
//! preserved; only the sizes the user chose change).
//!
//! The receiver maps every incoming chunk's source offset through this to its target offset, so
//! the map must be a bijection on the bytes it covers: no two source bytes share a target byte.

use crate::units::DiskRange;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutMap {
    /// Sorted by source start; validated to be a bijection.
    segments: Vec<(DiskRange, DiskRange)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("a segment is empty")]
    EmptySegment,
    #[error("a segment moves {source_len} source bytes to {target_len} target bytes")]
    LengthMismatch { source_len: u64, target_len: u64 },
    #[error("two segments overlap on the source disk")]
    SourceOverlap,
    #[error("two segments overlap on the target disk")]
    TargetOverlap,
    #[error("a segment reaches past the {len}-byte source disk")]
    SourceOutOfBounds { len: u64 },
    #[error("a segment reaches past the {len}-byte target disk")]
    TargetOutOfBounds { len: u64 },
}

impl LayoutMap {
    pub fn new(mut segments: Vec<(DiskRange, DiskRange)>) -> Self {
        segments.sort_by_key(|(source, _)| source.start);
        LayoutMap { segments }
    }

    pub fn segments(&self) -> &[(DiskRange, DiskRange)] {
        &self.segments
    }

    /// Checks the map is a well-formed bijection within both disks (M9).
    pub fn validate(&self, source_len: u64, target_len: u64) -> Result<(), LayoutError> {
        for (source, target) in &self.segments {
            if source.is_empty() || target.is_empty() {
                return Err(LayoutError::EmptySegment);
            }
            if source.len() != target.len() {
                return Err(LayoutError::LengthMismatch {
                    source_len: source.len(),
                    target_len: target.len(),
                });
            }
            if source.end > source_len {
                return Err(LayoutError::SourceOutOfBounds { len: source_len });
            }
            if target.end > target_len {
                return Err(LayoutError::TargetOutOfBounds { len: target_len });
            }
        }
        // Sources are already sorted by start; check they don't overlap.
        for pair in self.segments.windows(2) {
            if pair[0].0.end > pair[1].0.start {
                return Err(LayoutError::SourceOverlap);
            }
        }
        // Targets may be in any order; sort a copy and check.
        let mut targets: Vec<DiskRange> = self.segments.iter().map(|(_, t)| *t).collect();
        targets.sort_by_key(|t| t.start);
        for pair in targets.windows(2) {
            if pair[0].end > pair[1].start {
                return Err(LayoutError::TargetOverlap);
            }
        }
        Ok(())
    }

    /// The target offset a source byte lands at, or `None` if it is in no segment.
    pub fn map_offset(&self, source_offset: u64) -> Option<u64> {
        let (source, target) = self
            .segments
            .iter()
            .find(|(s, _)| s.start <= source_offset && source_offset < s.end)?;
        Some(target.start + (source_offset - source.start))
    }

    /// The target range a whole source range lands at, or `None` if it isn't contained in one
    /// segment (a chunk that straddles two partitions, which the reader never produces).
    pub fn map_range(&self, source: DiskRange) -> Option<DiskRange> {
        if source.is_empty() {
            return None;
        }
        let (seg_source, seg_target) = self
            .segments
            .iter()
            .find(|(s, _)| s.start <= source.start && source.end <= s.end)?;
        let start = seg_target.start + (source.start - seg_source.start);
        Some(DiskRange::new(start, start + source.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn seg(ss: u64, se: u64, ts: u64) -> (DiskRange, DiskRange) {
        (DiskRange::new(ss, se), DiskRange::new(ts, ts + (se - ss)))
    }

    #[test]
    fn a_simple_shift_maps_offsets_and_ranges() {
        let map = LayoutMap::new(vec![seg(0, 100, 0), seg(100, 200, 500)]);
        assert_eq!(map.validate(200, 700), Ok(()));
        assert_eq!(map.map_offset(50), Some(50));
        assert_eq!(map.map_offset(150), Some(550));
        assert_eq!(map.map_offset(999), None);
        assert_eq!(
            map.map_range(DiskRange::new(120, 140)),
            Some(DiskRange::new(520, 540))
        );
        // A range straddling two segments has no single target.
        assert_eq!(map.map_range(DiskRange::new(90, 110)), None);
    }

    #[test]
    fn overlaps_and_mismatches_are_rejected() {
        let overlap_source = LayoutMap::new(vec![seg(0, 100, 0), seg(50, 150, 500)]);
        assert_eq!(
            overlap_source.validate(200, 700),
            Err(LayoutError::SourceOverlap)
        );

        let overlap_target = LayoutMap::new(vec![seg(0, 100, 0), seg(100, 200, 50)]);
        assert_eq!(
            overlap_target.validate(200, 700),
            Err(LayoutError::TargetOverlap)
        );

        let bad_len = LayoutMap::new(vec![(DiskRange::new(0, 100), DiskRange::new(0, 90))]);
        assert!(matches!(
            bad_len.validate(200, 700),
            Err(LayoutError::LengthMismatch { .. })
        ));

        let past_target = LayoutMap::new(vec![seg(0, 100, 650)]);
        assert!(matches!(
            past_target.validate(200, 700),
            Err(LayoutError::TargetOutOfBounds { .. })
        ));
    }

    proptest! {
        /// A map built from non-overlapping, equal-length segments validates, and every source
        /// byte it covers maps to a distinct target byte (a bijection).
        #[test]
        fn a_valid_map_is_a_bijection(
            lengths in prop::collection::vec(1u64..20, 1..8),
            source_gaps in prop::collection::vec(0u64..5, 1..8),
            target_gaps in prop::collection::vec(0u64..5, 1..8),
        ) {
            let mut segments = Vec::new();
            let mut s_cursor = 0u64;
            let mut t_cursor = 0u64;
            for (i, &len) in lengths.iter().enumerate() {
                s_cursor += source_gaps.get(i).copied().unwrap_or(0);
                t_cursor += target_gaps.get(i).copied().unwrap_or(0);
                segments.push(seg(s_cursor, s_cursor + len, t_cursor));
                s_cursor += len;
                t_cursor += len;
            }
            let map = LayoutMap::new(segments);
            prop_assert_eq!(map.validate(s_cursor + 100, t_cursor + 100), Ok(()));

            // Every covered source byte maps to a distinct target byte.
            let mut seen = std::collections::BTreeSet::new();
            for (source, _) in map.segments() {
                for byte in source.start..source.end {
                    let target = map.map_offset(byte).expect("covered byte maps");
                    prop_assert!(seen.insert(target), "two source bytes share a target byte");
                }
            }
        }
    }
}
