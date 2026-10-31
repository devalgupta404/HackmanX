//! The on-disk format of the journal file.
//!
//! The file is preallocated on the ESP before the restart and then written in place:
//!
//! | File offset | Contents |
//! |---|---|
//! | 0 | progress slot A |
//! | 1 MiB | progress slot B |
//! | 2 MiB | plan record, written once before the restart |
//! | 3 MiB | scope record: the used ranges of the move in progress |
//!
//! Progress is committed to the two slots in turn, so a write torn by a power cut can only
//! damage the slot being written, never the newest complete one.
//!
//! A move's used ranges must be measured before its first chunk is copied, since afterwards
//! the source is partly overwritten. They are written to the scope record and flushed before
//! the slot that starts the copy is committed.

use std::ops::Range;

use crate::scope::CopyScope;

pub const SLOT_SIZE: usize = 4096;
pub const SLOT_OFFSETS: [u64; 2] = [0, 1 << 20];
pub const PLAN_OFFSET: u64 = 2 << 20;
pub const SCOPE_OFFSET: u64 = 3 << 20;
pub const FILE_LEN: u64 = 4 << 20;
pub const FORMAT_VERSION: u32 = 1;

const SLOT_MAGIC: [u8; 8] = *b"BSHFSLOT";
const PLAN_MAGIC: [u8; 8] = *b"BSHFPLAN";
const SCOPE_MAGIC: [u8; 8] = *b"BSHFSCOP";
const PLAN_HEADER_LEN: usize = 8 + 4 + 8 + 8;
const SCOPE_HEADER_LEN: usize = 8 + 4 + 8 + 4 + 1 + 4;
const CHECKSUM_LEN: usize = 4;
const RANGE_LEN: usize = 16;
const REGION_LEN: u64 = 1 << 20;

pub const MAX_PLAN_PAYLOAD: u64 = REGION_LEN - (PLAN_HEADER_LEN + CHECKSUM_LEN) as u64;
pub const MAX_SCOPE_RANGES: usize =
    (REGION_LEN as usize - SCOPE_HEADER_LEN - CHECKSUM_LEN) / RANGE_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Prepared = 1,
    Running = 2,
    Verifying = 3,
    Finishing = 4,
    Done = 5,
}

impl Stage {
    fn from_byte(byte: u8) -> Option<Stage> {
        [
            Stage::Prepared,
            Stage::Running,
            Stage::Verifying,
            Stage::Finishing,
            Stage::Done,
        ]
        .into_iter()
        .find(|stage| *stage as u8 == byte)
    }
}

/// How far the run has got. The executor defines the steps within each operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    pub operation: u32,
    pub steps_done: u8,
    pub next_chunk: u64,
    pub hash_chain: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub seq: u64,
    pub run_id: u64,
    pub progress: Progress,
}

/// The plan for one run. `run_id` is random per run and is repeated in every slot, so slots
/// from an earlier run are never mistaken for this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRecord {
    pub run_id: u64,
    pub payload: Vec<u8>,
}

/// The used ranges of one move, measured before its first chunk was copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRecord {
    pub run_id: u64,
    pub operation: u32,
    pub scope: CopyScope,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("not a Kitten journal record")]
    BadMagic,
    #[error("journal format version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("journal record is cut short")]
    Truncated,
    #[error("journal record is damaged")]
    BadChecksum,
    #[error("journal record has an unknown stage {0}")]
    UnknownStage(u8),
    #[error("journal plan of {0} bytes is larger than allowed")]
    PlanTooLarge(u64),
    #[error("journal scope holds {0} ranges, more than allowed")]
    TooManyRanges(u64),
    #[error("journal scope ranges are empty, unsorted or overlapping")]
    BadRanges,
}

impl Slot {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SLOT_SIZE);
        out.extend(SLOT_MAGIC);
        out.extend(FORMAT_VERSION.to_le_bytes());
        out.extend(self.seq.to_le_bytes());
        out.extend(self.run_id.to_le_bytes());
        out.push(self.progress.stage as u8);
        out.push(self.progress.steps_done);
        out.extend(self.progress.operation.to_le_bytes());
        out.extend(self.progress.next_chunk.to_le_bytes());
        out.extend(self.progress.hash_chain);
        out.extend(crc32c::crc32c(&out).to_le_bytes());
        out.resize(SLOT_SIZE, 0);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Slot, DecodeError> {
        let mut reader = Reader::new(bytes);
        reader.expect_magic(SLOT_MAGIC)?;
        reader.expect_version()?;
        let seq = reader.u64()?;
        let run_id = reader.u64()?;
        let stage = reader.u8()?;
        let steps_done = reader.u8()?;
        let operation = reader.u32()?;
        let next_chunk = reader.u64()?;
        let hash_chain = reader.array()?;
        reader.expect_checksum()?;

        Ok(Slot {
            seq,
            run_id,
            progress: Progress {
                stage: Stage::from_byte(stage).ok_or(DecodeError::UnknownStage(stage))?,
                operation,
                steps_done,
                next_chunk,
                hash_chain,
            },
        })
    }
}

impl PlanRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PLAN_HEADER_LEN + self.payload.len() + CHECKSUM_LEN);
        out.extend(PLAN_MAGIC);
        out.extend(FORMAT_VERSION.to_le_bytes());
        out.extend(self.run_id.to_le_bytes());
        out.extend((self.payload.len() as u64).to_le_bytes());
        out.extend(&self.payload);
        out.extend(crc32c::crc32c(&out).to_le_bytes());
        out
    }

    /// Reads just the header: the run ID and how many bytes the whole record takes.
    pub fn decode_header(bytes: &[u8]) -> Result<(u64, usize), DecodeError> {
        let mut reader = Reader::new(bytes);
        reader.expect_magic(PLAN_MAGIC)?;
        reader.expect_version()?;
        let run_id = reader.u64()?;
        let payload_len = reader.u64()?;
        if payload_len > MAX_PLAN_PAYLOAD {
            return Err(DecodeError::PlanTooLarge(payload_len));
        }
        Ok((
            run_id,
            PLAN_HEADER_LEN + payload_len as usize + CHECKSUM_LEN,
        ))
    }

    pub fn decode(bytes: &[u8]) -> Result<PlanRecord, DecodeError> {
        let (run_id, record_len) = Self::decode_header(bytes)?;
        let mut reader = Reader::new(bytes.get(..record_len).ok_or(DecodeError::Truncated)?);
        reader.skip(PLAN_HEADER_LEN)?;
        let payload = reader
            .bytes(record_len - PLAN_HEADER_LEN - CHECKSUM_LEN)?
            .to_vec();
        reader.expect_checksum()?;
        Ok(PlanRecord { run_id, payload })
    }
}

impl ScopeRecord {
    /// Encodes the record. A scope with more than [`MAX_SCOPE_RANGES`] ranges must be
    /// coalesced first.
    pub fn encode(&self) -> Vec<u8> {
        let (everything, ranges): (u8, &[Range<u64>]) = match &self.scope {
            CopyScope::Everything => (1, &[]),
            CopyScope::UsedOnly(ranges) => (0, ranges),
        };
        let mut out =
            Vec::with_capacity(SCOPE_HEADER_LEN + ranges.len() * RANGE_LEN + CHECKSUM_LEN);
        out.extend(SCOPE_MAGIC);
        out.extend(FORMAT_VERSION.to_le_bytes());
        out.extend(self.run_id.to_le_bytes());
        out.extend(self.operation.to_le_bytes());
        out.push(everything);
        out.extend((ranges.len() as u32).to_le_bytes());
        for range in ranges {
            out.extend(range.start.to_le_bytes());
            out.extend(range.end.to_le_bytes());
        }
        out.extend(crc32c::crc32c(&out).to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<ScopeRecord, DecodeError> {
        let mut reader = Reader::new(bytes);
        reader.expect_magic(SCOPE_MAGIC)?;
        reader.expect_version()?;
        let run_id = reader.u64()?;
        let operation = reader.u32()?;
        let everything = reader.u8()? == 1;
        let count = u64::from(reader.u32()?);
        if count > MAX_SCOPE_RANGES as u64 {
            return Err(DecodeError::TooManyRanges(count));
        }
        let mut ranges = Vec::with_capacity(count as usize);
        for _ in 0..count {
            ranges.push(reader.u64()?..reader.u64()?);
        }
        reader.expect_checksum()?;

        let sorted_and_disjoint = ranges.iter().all(|r| r.start < r.end)
            && ranges.windows(2).all(|pair| pair[0].end <= pair[1].start);
        if !sorted_and_disjoint {
            return Err(DecodeError::BadRanges);
        }
        let scope = if everything {
            CopyScope::Everything
        } else {
            CopyScope::UsedOnly(ranges)
        };
        Ok(ScopeRecord {
            run_id,
            operation,
            scope,
        })
    }
}

/// The bytes the journal file starts with: empty slots and the plan record.
pub fn initial_file_contents(plan: &PlanRecord) -> Vec<u8> {
    let mut contents = vec![0; FILE_LEN as usize];
    let record = plan.encode();
    let start = PLAN_OFFSET as usize;
    contents[start..start + record.len()].copy_from_slice(&record);
    contents
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(DecodeError::Truncated)?;
        let taken = self
            .bytes
            .get(self.position..end)
            .ok_or(DecodeError::Truncated)?;
        self.position = end;
        Ok(taken)
    }

    fn skip(&mut self, len: usize) -> Result<(), DecodeError> {
        self.bytes(len).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.bytes(N)?
            .try_into()
            .map_err(|_| DecodeError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }

    fn expect_magic(&mut self, magic: [u8; 8]) -> Result<(), DecodeError> {
        if self.array()? == magic {
            Ok(())
        } else {
            Err(DecodeError::BadMagic)
        }
    }

    fn expect_version(&mut self) -> Result<(), DecodeError> {
        match self.u32()? {
            FORMAT_VERSION => Ok(()),
            other => Err(DecodeError::UnsupportedVersion(other)),
        }
    }

    /// Checks the CRC32C stored after everything read so far.
    fn expect_checksum(&mut self) -> Result<(), DecodeError> {
        let covered = &self.bytes[..self.position];
        let expected = crc32c::crc32c(covered);
        if self.u32()? == expected {
            Ok(())
        } else {
            Err(DecodeError::BadChecksum)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> Slot {
        Slot {
            seq: 7,
            run_id: 0xDEAD_BEEF,
            progress: Progress {
                stage: Stage::Running,
                operation: 3,
                steps_done: 1,
                next_chunk: 42,
                hash_chain: [9; 32],
            },
        }
    }

    fn plan() -> PlanRecord {
        PlanRecord {
            run_id: 0xDEAD_BEEF,
            payload: b"operations".to_vec(),
        }
    }

    #[test]
    fn slot_round_trips_and_fills_one_block() {
        let encoded = slot().encode();
        assert_eq!(encoded.len(), SLOT_SIZE);
        assert_eq!(Slot::decode(&encoded), Ok(slot()));
    }

    #[test]
    fn every_single_bit_flip_in_a_slot_is_detected() {
        let encoded = slot().encode();
        let used_len = encoded.iter().rposition(|&b| b != 0).unwrap_or(0) + 1;
        for bit in 0..used_len * 8 {
            let mut damaged = encoded.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(
                Slot::decode(&damaged).is_err(),
                "bit {bit} flip went unnoticed"
            );
        }
    }

    #[test]
    fn zeroed_slot_is_not_a_record() {
        assert_eq!(Slot::decode(&[0; SLOT_SIZE]), Err(DecodeError::BadMagic));
    }

    #[test]
    fn slot_from_a_newer_format_is_refused() {
        let mut encoded = slot().encode();
        encoded[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            Slot::decode(&encoded),
            Err(DecodeError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn version_1_slot_layout_stays_readable() {
        let mut bytes = Vec::new();
        bytes.extend(b"BSHFSLOT");
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(7u64.to_le_bytes());
        bytes.extend(0xDEAD_BEEFu64.to_le_bytes());
        bytes.extend([2, 1]);
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(42u64.to_le_bytes());
        bytes.extend([9; 32]);
        bytes.extend(crc32c::crc32c(&bytes).to_le_bytes());

        assert_eq!(Slot::decode(&bytes), Ok(slot()));
    }

    #[test]
    fn plan_round_trips() {
        assert_eq!(PlanRecord::decode(&plan().encode()), Ok(plan()));
    }

    #[test]
    fn plan_header_gives_run_id_and_record_length() {
        let encoded = plan().encode();
        assert_eq!(
            PlanRecord::decode_header(&encoded),
            Ok((0xDEAD_BEEF, encoded.len()))
        );
    }

    #[test]
    fn damaged_plan_is_detected() {
        let mut encoded = plan().encode();
        encoded[PLAN_HEADER_LEN] ^= 1;
        assert_eq!(PlanRecord::decode(&encoded), Err(DecodeError::BadChecksum));
    }

    #[test]
    fn oversized_plan_length_is_refused_before_reading_it() {
        let mut encoded = plan().encode();
        encoded[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            PlanRecord::decode_header(&encoded),
            Err(DecodeError::PlanTooLarge(u64::MAX))
        );
    }

    fn scope_record() -> ScopeRecord {
        ScopeRecord {
            run_id: 0xDEAD_BEEF,
            operation: 2,
            scope: CopyScope::UsedOnly(vec![0..4096, 1 << 20..3 << 20]),
        }
    }

    #[test]
    fn scope_record_round_trips() {
        assert_eq!(
            ScopeRecord::decode(&scope_record().encode()),
            Ok(scope_record())
        );
        let everything = ScopeRecord {
            scope: CopyScope::Everything,
            ..scope_record()
        };
        assert_eq!(ScopeRecord::decode(&everything.encode()), Ok(everything));
    }

    #[test]
    fn every_single_bit_flip_in_a_scope_record_is_detected() {
        let encoded = scope_record().encode();
        for bit in 0..encoded.len() * 8 {
            let mut damaged = encoded.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(
                ScopeRecord::decode(&damaged).is_err(),
                "bit {bit} flip went unnoticed"
            );
        }
    }

    #[test]
    fn overlapping_scope_ranges_are_refused() {
        let record = ScopeRecord {
            scope: CopyScope::UsedOnly(vec![0..100, 50..200]),
            ..scope_record()
        };
        assert_eq!(
            ScopeRecord::decode(&record.encode()),
            Err(DecodeError::BadRanges)
        );
    }

    #[test]
    fn the_largest_records_stay_inside_their_regions() {
        let biggest_plan = PLAN_HEADER_LEN as u64 + MAX_PLAN_PAYLOAD + CHECKSUM_LEN as u64;
        assert!(PLAN_OFFSET + biggest_plan <= SCOPE_OFFSET);
        let biggest_scope = SCOPE_HEADER_LEN + MAX_SCOPE_RANGES * RANGE_LEN + CHECKSUM_LEN;
        assert!(SCOPE_OFFSET + biggest_scope as u64 <= FILE_LEN);
    }

    #[test]
    fn initial_contents_hold_the_plan_and_empty_slots() {
        let contents = initial_file_contents(&plan());
        assert_eq!(contents.len() as u64, FILE_LEN);
        assert_eq!(Slot::decode(&contents), Err(DecodeError::BadMagic));
        assert_eq!(
            PlanRecord::decode(&contents[PLAN_OFFSET as usize..]),
            Ok(plan())
        );
    }
}
