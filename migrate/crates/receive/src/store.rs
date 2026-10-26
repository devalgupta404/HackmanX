//! The receiver's scratch store (architecture A4.1, implementation I4): two alternating journal
//! slots and the hash index, written raw to the scratch device with the same discipline as
//! `bootshift-core`'s journal — a torn write damages only the slot being written, and the newest
//! valid slot wins. The index holds one blake3 per 1 MiB target chunk, so the Merkle root is cheap
//! to recompute (A4.3).
//!
//! Formats are versioned; when one changes, its version is bumped and a decode test for the old
//! version is kept (the repository's journal rule, applied here).

use bootshift_disk::{BlockDevice, DiskError};
use bootshift_migrate_core::{Epoch, FrameSeq};

/// The two slots and the index, at fixed offsets on the scratch device.
const SLOT_A: u64 = 0;
const SLOT_B: u64 = 1 << 20;
const INDEX_OFFSET: u64 = 16 << 20;

/// One journal record is a 4 KiB block.
const SLOT_BYTES: usize = 4096;
const MAGIC: &[u8; 8] = b"BSMGSLOT";
const FORMAT_VERSION: u16 = 1;

/// The size in bytes of the target chunk each index entry covers.
pub const INDEX_CHUNK: u64 = 1 << 20;
const ENTRY_BYTES: u64 = 32;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("scratch device I/O")]
    Disk(#[from] DiskError),
    #[error("no valid journal slot on the scratch device")]
    NoSlot,
}

/// The receiver's durable run state (A4.1). `state` is a [`ReceiverState`](bootshift_migrate_core::ReceiverState)
/// encoded as a byte; the applier only needs `acked_seq` and `index_root`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub seq: u64,
    pub run_id: u64,
    pub epoch: Epoch,
    pub state: u8,
    pub acked_seq: FrameSeq,
    pub index_root: [u8; 32],
}

impl Slot {
    fn encode(&self) -> [u8; SLOT_BYTES] {
        let mut block = [0u8; SLOT_BYTES];
        block[0..8].copy_from_slice(MAGIC);
        block[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        block[10..18].copy_from_slice(&self.seq.to_le_bytes());
        block[18..26].copy_from_slice(&self.run_id.to_le_bytes());
        block[26..34].copy_from_slice(&self.epoch.0.to_le_bytes());
        block[34] = self.state;
        block[35..43].copy_from_slice(&self.acked_seq.0.to_le_bytes());
        block[43..75].copy_from_slice(&self.index_root);
        let crc = crc32c::crc32c(&block[..SLOT_BYTES - 4]);
        block[SLOT_BYTES - 4..].copy_from_slice(&crc.to_le_bytes());
        block
    }

    fn decode(block: &[u8]) -> Option<Slot> {
        if block.len() < SLOT_BYTES || &block[0..8] != MAGIC {
            return None;
        }
        let version = u16::from_le_bytes([block[8], block[9]]);
        if version != FORMAT_VERSION {
            return None;
        }
        let stored_crc = u32::from_le_bytes([
            block[SLOT_BYTES - 4],
            block[SLOT_BYTES - 3],
            block[SLOT_BYTES - 2],
            block[SLOT_BYTES - 1],
        ]);
        if crc32c::crc32c(&block[..SLOT_BYTES - 4]) != stored_crc {
            return None;
        }
        let u64_at = |at: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&block[at..at + 8]);
            u64::from_le_bytes(bytes)
        };
        let mut index_root = [0u8; 32];
        index_root.copy_from_slice(&block[43..75]);
        Some(Slot {
            seq: u64_at(10),
            run_id: u64_at(18),
            epoch: Epoch(u64_at(26)),
            state: block[34],
            acked_seq: FrameSeq(u64_at(35)),
            index_root,
        })
    }
}

/// Reads and writes the scratch device.
pub struct Store<'a> {
    device: &'a mut dyn BlockDevice,
}

impl<'a> Store<'a> {
    pub fn new(device: &'a mut dyn BlockDevice) -> Self {
        Store { device }
    }

    /// Writes `slot` to the A or B position (by seq parity), then flushes. Alternating by parity
    /// means the previous slot is always the other one, so a torn write never loses both.
    pub fn commit(&mut self, slot: Slot) -> Result<(), StoreError> {
        let at = if slot.seq % 2 == 0 { SLOT_A } else { SLOT_B };
        self.device.write_at(at, &slot.encode())?;
        self.device.flush()?;
        Ok(())
    }

    /// The newest valid slot, or `None` if neither is valid (a fresh device).
    pub fn read_newest(&self) -> Result<Option<Slot>, StoreError> {
        let a = self.read_slot(SLOT_A)?;
        let b = self.read_slot(SLOT_B)?;
        Ok(match (a, b) {
            (Some(a), Some(b)) => Some(if a.seq >= b.seq { a } else { b }),
            (Some(a), None) => Some(a),
            (None, b) => b,
        })
    }

    fn read_slot(&self, at: u64) -> Result<Option<Slot>, StoreError> {
        let mut block = [0u8; SLOT_BYTES];
        self.device.read_at(at, &mut block)?;
        Ok(Slot::decode(&block))
    }

    /// The stored hash of target chunk `chunk`.
    pub fn chunk_hash(&self, chunk: u64) -> Result<[u8; 32], StoreError> {
        let (block_offset, within) = self.entry_location(chunk);
        let mut block = [0u8; SLOT_BYTES];
        self.device.read_at(block_offset, &mut block)?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&block[within..within + 32]);
        Ok(hash)
    }

    /// Stores the hash of target chunk `chunk`, read-modify-writing its 4 KiB index block (the
    /// device only writes whole blocks), then flushing.
    pub fn set_chunk_hash(&mut self, chunk: u64, hash: [u8; 32]) -> Result<(), StoreError> {
        let (block_offset, within) = self.entry_location(chunk);
        let mut block = [0u8; SLOT_BYTES];
        self.device.read_at(block_offset, &mut block)?;
        block[within..within + 32].copy_from_slice(&hash);
        self.device.write_at(block_offset, &block)?;
        self.device.flush()?;
        Ok(())
    }

    /// The 4 KiB block holding chunk `chunk`'s entry, and the byte offset within it.
    fn entry_location(&self, chunk: u64) -> (u64, usize) {
        let entry = INDEX_OFFSET + chunk * ENTRY_BYTES;
        let block_offset = entry / SLOT_BYTES as u64 * SLOT_BYTES as u64;
        let within = (entry - block_offset) as usize;
        (block_offset, within)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootshift_disk::MemDisk;

    fn scratch() -> MemDisk {
        // 32 MiB is enough for the slots and a small index.
        MemDisk::zeroed(32 << 20, 4096).expect("scratch disk")
    }

    fn slot(seq: u64, acked: u64) -> Slot {
        Slot {
            seq,
            run_id: 1234,
            epoch: Epoch(7),
            state: 2,
            acked_seq: FrameSeq(acked),
            index_root: [seq as u8; 32],
        }
    }

    #[test]
    fn a_slot_round_trips() {
        assert_eq!(Slot::decode(&slot(3, 99).encode()), Some(slot(3, 99)));
    }

    #[test]
    fn a_torn_slot_is_ignored() {
        let mut block = slot(1, 1).encode();
        block[100] ^= 0xff; // flip a byte
        assert_eq!(Slot::decode(&block), None);
    }

    #[test]
    fn a_wrong_version_is_ignored() {
        let mut block = slot(1, 1).encode();
        block[8] = 2; // version 2, not 1
        // The CRC still has to be fixed for a fair test, but a wrong version is rejected first.
        let crc = crc32c::crc32c(&block[..SLOT_BYTES - 4]);
        block[SLOT_BYTES - 4..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Slot::decode(&block), None);
    }

    #[test]
    fn the_newest_valid_slot_wins() {
        let mut disk = scratch();
        let mut store = Store::new(&mut disk);
        store.commit(slot(0, 10)).expect("commit a"); // seq 0 → slot A
        store.commit(slot(1, 20)).expect("commit b"); // seq 1 → slot B
        assert_eq!(store.read_newest().expect("read"), Some(slot(1, 20)));
        store.commit(slot(2, 30)).expect("commit a again"); // seq 2 → slot A
        assert_eq!(store.read_newest().expect("read"), Some(slot(2, 30)));
    }

    #[test]
    fn a_corrupt_newest_slot_falls_back_to_the_other() {
        let mut disk = scratch();
        {
            let mut store = Store::new(&mut disk);
            store.commit(slot(0, 10)).expect("A"); // slot A, seq 0
            store.commit(slot(1, 20)).expect("B"); // slot B, seq 1 (newest)
        }
        // Corrupt slot B in place.
        let mut block = [0u8; SLOT_BYTES];
        disk.read_at(SLOT_B, &mut block).expect("read B");
        block[50] ^= 0xff;
        disk.write_at(SLOT_B, &block).expect("write B");
        let store = Store::new(&mut disk);
        assert_eq!(store.read_newest().expect("read"), Some(slot(0, 10)));
    }

    #[test]
    fn a_fresh_device_has_no_slot() {
        let mut disk = scratch();
        let store = Store::new(&mut disk);
        assert_eq!(store.read_newest().expect("read"), None);
    }

    #[test]
    fn index_entries_round_trip_including_neighbours_in_the_same_block() {
        let mut disk = scratch();
        let mut store = Store::new(&mut disk);
        // Chunks 0 and 1 share the first index block; chunk 200 is in a later one.
        store.set_chunk_hash(0, [1; 32]).expect("set 0");
        store.set_chunk_hash(1, [2; 32]).expect("set 1");
        store.set_chunk_hash(200, [3; 32]).expect("set 200");
        assert_eq!(store.chunk_hash(0).expect("get 0"), [1; 32]);
        assert_eq!(store.chunk_hash(1).expect("get 1"), [2; 32]);
        assert_eq!(store.chunk_hash(200).expect("get 200"), [3; 32]);
    }
}
