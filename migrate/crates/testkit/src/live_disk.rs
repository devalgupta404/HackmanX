//! [`LiveDisk`]: a [`MemDisk`] a deterministic, seeded writer mutates, recording every write it
//! makes as a ground-truth log. The end-to-end simulation ticks it between the migration's reads
//! to model a busy source, then checks the migrated target equals the disk's bytes at the freeze
//! point. The same seed always reproduces the same history, so any failure is replayable.

use bootshift_disk::{BlockDevice, DiskError, MemDisk};
use bootshift_migrate_core::DiskRange;

/// How many blocks one seeded write touches, at most.
const MAX_WRITE_BLOCKS: u64 = 8;

pub struct LiveDisk {
    inner: MemDisk,
    rng: SplitMix,
    block_size: u32,
    /// Every write applied, in order: the ground truth a tracker is measured against.
    log: Vec<DiskRange>,
}

impl LiveDisk {
    pub fn new(len_bytes: usize, block_size: u32, seed: u64) -> Result<Self, DiskError> {
        Ok(LiveDisk {
            inner: MemDisk::zeroed(len_bytes, block_size)?,
            rng: SplitMix::new(seed),
            block_size,
            log: Vec::new(),
        })
    }

    /// Applies the next seeded write and records it. Returns the range written.
    pub fn tick(&mut self) -> Result<DiskRange, DiskError> {
        let blocks = self.inner.len_bytes() / u64::from(self.block_size);
        let start_block = self.rng.next() % blocks;
        let room = (blocks - start_block).min(MAX_WRITE_BLOCKS);
        let count = 1 + self.rng.next() % room.max(1);
        let offset = start_block * u64::from(self.block_size);
        let len = (count * u64::from(self.block_size)) as usize;
        let mut data = vec![0u8; len];
        for byte in data.iter_mut() {
            *byte = (self.rng.next() & 0xff) as u8;
        }
        self.inner.write_at(offset, &data)?;
        let range = DiskRange::new(offset, offset + len as u64);
        self.log.push(range);
        Ok(range)
    }

    /// The ordered list of every write so far.
    pub fn log(&self) -> &[DiskRange] {
        &self.log
    }

    /// A copy of the disk's bytes right now: the image a cutover at this moment must reproduce.
    pub fn snapshot(&self) -> Vec<u8> {
        self.inner.bytes().to_vec()
    }
}

impl BlockDevice for LiveDisk {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }

    fn logical_block_size(&self) -> u32 {
        self.inner.logical_block_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        self.inner.write_at(offset, data)
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.inner.flush()
    }
}

/// SplitMix64: a tiny, seedable PRNG. Deterministic, no external crate, so seeds replay exactly.
struct SplitMix {
    state: u64,
}

impl SplitMix {
    fn new(seed: u64) -> Self {
        SplitMix {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_reproduces_the_exact_same_history() {
        let mut a = LiveDisk::new(64 * 1024, 4096, 1234).expect("disk a");
        let mut b = LiveDisk::new(64 * 1024, 4096, 1234).expect("disk b");
        for _ in 0..200 {
            assert_eq!(a.tick().expect("tick a"), b.tick().expect("tick b"));
        }
        assert_eq!(a.log(), b.log());
        assert_eq!(a.snapshot(), b.snapshot());
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = LiveDisk::new(64 * 1024, 4096, 1).expect("disk a");
        let mut b = LiveDisk::new(64 * 1024, 4096, 2).expect("disk b");
        for _ in 0..50 {
            a.tick().expect("tick a");
            b.tick().expect("tick b");
        }
        assert_ne!(a.snapshot(), b.snapshot());
    }

    #[test]
    fn writes_stay_aligned_and_in_bounds() {
        let mut disk = LiveDisk::new(32 * 1024, 4096, 99).expect("disk");
        for _ in 0..500 {
            let range = disk.tick().expect("tick");
            assert_eq!(range.start % 4096, 0);
            assert_eq!(range.end % 4096, 0);
            assert!(range.end <= disk.len_bytes());
            assert!(!range.is_empty());
        }
    }

    #[test]
    fn reading_back_a_write_sees_its_bytes() {
        let mut disk = LiveDisk::new(16 * 1024, 4096, 7).expect("disk");
        let range = disk.tick().expect("tick");
        let mut buf = vec![0u8; range.len() as usize];
        disk.read_at(range.start, &mut buf).expect("read");
        // The snapshot and a direct read of the same range must agree.
        let snap = disk.snapshot();
        assert_eq!(&buf[..], &snap[range.start as usize..range.end as usize]);
    }
}
