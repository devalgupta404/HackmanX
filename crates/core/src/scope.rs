use std::ops::Range;

use serde::{Deserialize, Serialize};

/// The start and end of a partition hold boot sectors and backup superblocks that some
/// free-space maps don't mark as used (NTFS keeps its backup boot sector in the last sector).
const ALWAYS_COPIED: u64 = 1 << 20;

/// Which bytes of a partition hold data, relative to its start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyScope {
    Everything,
    /// Sorted, non-overlapping ranges.
    UsedOnly(Vec<Range<u64>>),
}

impl CopyScope {
    /// Builds the scope from a filesystem's used ranges, always adding the first and last MiB.
    pub fn used_only(partition_len: u64, used: impl IntoIterator<Item = Range<u64>>) -> CopyScope {
        let mut ranges: Vec<Range<u64>> = used
            .into_iter()
            .map(|r| r.start.min(partition_len)..r.end.min(partition_len))
            .collect();
        ranges.push(0..ALWAYS_COPIED.min(partition_len));
        ranges.push(partition_len.saturating_sub(ALWAYS_COPIED)..partition_len);
        CopyScope::UsedOnly(merge(ranges))
    }

    /// The same scope with at most `max_ranges` ranges, made by filling the smallest gaps.
    /// Filling a gap only copies some free space as well, which is always safe.
    pub fn coalesced(self, max_ranges: usize) -> CopyScope {
        let CopyScope::UsedOnly(mut ranges) = self else {
            return self;
        };
        if max_ranges == 0 {
            return CopyScope::Everything;
        }
        while ranges.len() > max_ranges {
            let Some(smallest_gap) =
                (1..ranges.len()).min_by_key(|&i| ranges[i].start - ranges[i - 1].end)
            else {
                break;
            };
            ranges[smallest_gap - 1].end = ranges[smallest_gap].end;
            ranges.remove(smallest_gap);
        }
        CopyScope::UsedOnly(ranges)
    }
}

/// Sorts ranges and joins those that overlap or touch, dropping empty ones.
pub fn merge(mut ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn used_scope_always_covers_the_first_and_last_mib() {
        let scope = CopyScope::used_only(10 * MIB, [5 * MIB..5 * MIB + 10]);
        assert_eq!(
            scope,
            CopyScope::UsedOnly(vec![0..MIB, 5 * MIB..5 * MIB + 10, 9 * MIB..10 * MIB])
        );
    }

    #[test]
    fn coalescing_fills_the_smallest_gaps_first() {
        let scope = CopyScope::UsedOnly(vec![0..10, 12..20, 100..110, 111..120]);
        assert_eq!(
            scope.coalesced(2),
            CopyScope::UsedOnly(vec![0..20, 100..120])
        );
    }

    #[test]
    fn coalesced_scope_still_covers_every_original_range() {
        let original = vec![0..5, 9..14, 30..31, 50..60, 61..70];
        let CopyScope::UsedOnly(coalesced) = CopyScope::UsedOnly(original.clone()).coalesced(2)
        else {
            panic!("coalescing turned used ranges into everything");
        };
        for range in original {
            assert!(
                coalesced
                    .iter()
                    .any(|c| c.start <= range.start && range.end <= c.end)
            );
        }
    }
}
