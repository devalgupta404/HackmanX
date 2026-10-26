use std::ops::RangeInclusive;

use crate::device::{BlockDevice, DiskError, checked_range};
use crate::mem_disk::MemDisk;

/// The unit a drive persists atomically. A write cut off by power loss can tear at this size.
const TORN_WRITE_UNIT: usize = 512;

/// What happens to writes still in the drive's volatile cache when the power goes out.
#[derive(Clone, Copy, Debug)]
pub enum PendingWrites {
    Lost,
    AllLanded,
    /// Each 512-byte piece of every pending write lands or not, independently and in a shuffled
    /// order. This covers dropped, reordered and torn writes. A seed always gives the same image.
    Scrambled {
        seed: u64,
    },
}

enum Op {
    Write { start: usize, data: Vec<u8> },
    Flush,
}

/// A disk with a volatile write cache, for crash testing.
///
/// Writes stay in the cache until `flush`. Every write and flush is recorded, so a test can
/// rebuild the disk as a power cut would leave it after any number of operations.
pub struct CrashDisk {
    initial: Vec<u8>,
    cached_view: Vec<u8>,
    block_size: u32,
    ops: Vec<Op>,
}

impl CrashDisk {
    pub fn new(initial: MemDisk) -> Self {
        let block_size = initial.logical_block_size();
        let initial = initial.into_bytes();
        Self {
            cached_view: initial.clone(),
            initial,
            block_size,
            ops: Vec::new(),
        }
    }

    /// Every point a crash can happen: point `n` means the power went out after the first `n`
    /// recorded operations, so a flush at index `n` had not completed.
    pub fn crash_points(&self) -> RangeInclusive<usize> {
        0..=self.ops.len()
    }

    /// The disk as it would be after a power cut at `point`. Points past the end mean after the
    /// last operation.
    pub fn crash_image(&self, point: usize, pending: PendingWrites) -> MemDisk {
        let ops = &self.ops[..point.min(self.ops.len())];
        let (flushed, unflushed) = match ops.iter().rposition(|op| matches!(op, Op::Flush)) {
            Some(last_flush) => ops.split_at(last_flush + 1),
            None => (&[][..], ops),
        };

        let mut image = self.initial.clone();
        for (start, data) in writes(flushed) {
            apply(&mut image, start, data);
        }
        match pending {
            PendingWrites::Lost => {}
            PendingWrites::AllLanded => {
                for (start, data) in writes(unflushed) {
                    apply(&mut image, start, data);
                }
            }
            PendingWrites::Scrambled { seed } => scramble_onto(&mut image, unflushed, seed),
        }
        MemDisk::from_checked_parts(image, self.block_size)
    }
}

impl BlockDevice for CrashDisk {
    fn len_bytes(&self) -> u64 {
        self.cached_view.len() as u64
    }

    fn logical_block_size(&self) -> u32 {
        self.block_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        let range = checked_range(offset, buf.len(), self.len_bytes(), self.block_size)?;
        buf.copy_from_slice(&self.cached_view[range]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        let range = checked_range(offset, data.len(), self.len_bytes(), self.block_size)?;
        self.cached_view[range.clone()].copy_from_slice(data);
        self.ops.push(Op::Write {
            start: range.start,
            data: data.to_vec(),
        });
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.ops.push(Op::Flush);
        Ok(())
    }
}

fn writes(ops: &[Op]) -> impl Iterator<Item = (usize, &[u8])> {
    ops.iter().filter_map(|op| match op {
        Op::Write { start, data } => Some((*start, data.as_slice())),
        Op::Flush => None,
    })
}

fn apply(image: &mut [u8], start: usize, data: &[u8]) {
    image[start..start + data.len()].copy_from_slice(data);
}

fn scramble_onto(image: &mut [u8], unflushed: &[Op], seed: u64) {
    let mut pieces: Vec<(usize, &[u8])> = writes(unflushed)
        .flat_map(|(start, data)| {
            data.chunks(TORN_WRITE_UNIT)
                .enumerate()
                .map(move |(i, piece)| (start + i * TORN_WRITE_UNIT, piece))
        })
        .collect();

    let mut rng = SplitMix64(seed);
    for i in (1..pieces.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        pieces.swap(i, j);
    }
    for (start, piece) in pieces {
        if rng.next() & 1 == 1 {
            apply(image, start, piece);
        }
    }
}

// Our own generator, so a printed seed reproduces the same crash on any toolchain or
// dependency version.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const SECTOR: usize = 512;

    fn blank_disk() -> CrashDisk {
        CrashDisk::new(MemDisk::zeroed(8 * SECTOR, 512).unwrap())
    }

    fn sector(image: &MemDisk, index: usize) -> &[u8] {
        &image.bytes()[index * SECTOR..(index + 1) * SECTOR]
    }

    #[test]
    fn reads_see_writes_still_in_the_cache() {
        let mut disk = blank_disk();
        disk.write_at(0, &[9; SECTOR]).unwrap();

        let mut buf = [0; SECTOR];
        disk.read_at(0, &mut buf).unwrap();

        assert_eq!(buf, [9; SECTOR]);
    }

    #[test]
    fn unflushed_write_is_gone_when_pending_writes_are_lost() {
        let mut disk = blank_disk();
        disk.write_at(0, &[9; SECTOR]).unwrap();

        let image = disk.crash_image(1, PendingWrites::Lost);

        assert_eq!(sector(&image, 0), &[0; SECTOR]);
    }

    #[test]
    fn flushed_write_survives_every_crash_outcome() {
        let mut disk = blank_disk();
        disk.write_at(0, &[9; SECTOR]).unwrap();
        disk.flush().unwrap();
        disk.write_at(0, &[5; SECTOR]).unwrap();

        let image = disk.crash_image(2, PendingWrites::Lost);
        assert_eq!(sector(&image, 0), &[9; SECTOR]);
        for seed in 0..50 {
            let image = disk.crash_image(2, PendingWrites::Scrambled { seed });
            assert_eq!(sector(&image, 0), &[9; SECTOR], "seed {seed}");
        }
    }

    #[test]
    fn crash_before_a_flush_completes_leaves_its_writes_pending() {
        let mut disk = blank_disk();
        disk.write_at(0, &[9; SECTOR]).unwrap();
        disk.flush().unwrap();

        let before_flush = disk.crash_image(1, PendingWrites::Lost);
        let after_flush = disk.crash_image(2, PendingWrites::Lost);

        assert_eq!(sector(&before_flush, 0), &[0; SECTOR]);
        assert_eq!(sector(&after_flush, 0), &[9; SECTOR]);
    }

    #[test]
    fn all_landed_image_matches_what_reads_returned() {
        let mut disk = blank_disk();
        disk.write_at(0, &[1; 2 * SECTOR]).unwrap();
        disk.write_at(SECTOR as u64, &[2; SECTOR]).unwrap();

        let image = disk.crash_image(2, PendingWrites::AllLanded);

        assert_eq!(image.bytes(), disk.cached_view.as_slice());
    }

    #[test]
    fn scrambled_crash_can_leave_any_cached_version_of_a_sector() {
        let mut disk = blank_disk();
        disk.write_at(0, &[1; SECTOR]).unwrap();
        disk.write_at(0, &[2; SECTOR]).unwrap();

        let seen: HashSet<u8> = (0..200)
            .map(|seed| {
                disk.crash_image(2, PendingWrites::Scrambled { seed })
                    .bytes()[0]
            })
            .collect();

        assert_eq!(seen, HashSet::from([0, 1, 2]));
    }

    #[test]
    fn scrambled_crash_can_tear_a_multi_sector_write() {
        let mut disk = blank_disk();
        disk.write_at(0, &[7; 8 * SECTOR]).unwrap();

        let torn = (0..50).any(|seed| {
            let image = disk.crash_image(1, PendingWrites::Scrambled { seed });
            let landed = (0..8).filter(|&i| sector(&image, i) == [7; SECTOR]).count();
            landed > 0 && landed < 8
        });

        assert!(torn);
    }

    #[test]
    fn same_seed_gives_the_same_image() {
        let mut disk = blank_disk();
        disk.write_at(0, &[3; 8 * SECTOR]).unwrap();

        let first = disk.crash_image(1, PendingWrites::Scrambled { seed: 42 });
        let second = disk.crash_image(1, PendingWrites::Scrambled { seed: 42 });

        assert!(first == second);
    }

    #[test]
    fn crash_points_run_from_before_the_first_op_to_after_the_last() {
        let mut disk = blank_disk();
        disk.write_at(0, &[1; SECTOR]).unwrap();
        disk.flush().unwrap();

        assert_eq!(disk.crash_points(), 0..=2);
    }

    #[test]
    fn rejected_write_is_not_recorded() {
        let mut disk = blank_disk();
        assert!(disk.write_at(3, &[1; SECTOR]).is_err());
        assert_eq!(disk.crash_points(), 0..=0);
    }
}
