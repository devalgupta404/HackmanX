//! An append-only log of the original contents of blocks, used to roll a guarded run back.
//!
//! Each record holds one run of blocks and is checksummed on its own. Records are read in
//! order until the first one that is missing, damaged, or belongs to another guarded run, so a
//! record torn by a power cut simply ends the log.

use crate::device::{BlockDevice, DiskError};
use crate::journal_store::{JournalError, JournalFile};

const MAGIC: [u8; 8] = *b"BSHFUNDO";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 8 + 4 + 8 + 8 + 8 + 4;
const CHECKSUM_LEN: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoRecord {
    pub offset: u64,
    pub original: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum UndoError {
    #[error(transparent)]
    Disk(#[from] DiskError),
    #[error(transparent)]
    Area(#[from] JournalError),
    #[error("the undo area is full; it needs more than {0} bytes")]
    Full(u64),
    #[error("the undo area lies inside the partition it is meant to protect")]
    AreaInsideWindow,
}

impl UndoError {
    /// Passes undo failures through the `BlockDevice` interface the tool writes through.
    pub(crate) fn into_disk_error(self) -> DiskError {
        match self {
            UndoError::Disk(error) => error,
            other => DiskError::Io {
                offset: 0,
                source: std::io::Error::other(other.to_string()),
            },
        }
    }
}

/// The log, stored in a preallocated area given by its extents on the device.
pub struct UndoLog {
    area: JournalFile,
    area_len: u64,
    guard_id: u64,
    next_seq: u64,
    write_position: u64,
}

impl UndoLog {
    /// Starts an empty log for the guarded run `guard_id`, overwriting whatever was there.
    pub fn create(area: JournalFile, area_len: u64, guard_id: u64) -> Self {
        Self {
            area,
            area_len,
            guard_id,
            next_seq: 0,
            write_position: 0,
        }
    }

    /// Appends a record. It is durable only after the device is flushed.
    pub fn append<D: BlockDevice + ?Sized>(
        &mut self,
        device: &mut D,
        offset: u64,
        original: &[u8],
    ) -> Result<(), UndoError> {
        let block = u64::from(device.logical_block_size());
        let record = encode(self.guard_id, self.next_seq, offset, original, block);
        let end = self.write_position + record.len() as u64;
        if end > self.area_len {
            return Err(UndoError::Full(self.area_len));
        }
        self.area.write_at(device, self.write_position, &record)?;
        self.write_position = end;
        self.next_seq += 1;
        Ok(())
    }

    /// Every complete record of `guard_id`, in the order they were written.
    pub fn read_all<D: BlockDevice + ?Sized>(
        device: &D,
        area: &JournalFile,
        area_len: u64,
        guard_id: u64,
    ) -> Result<Vec<UndoRecord>, UndoError> {
        let block = u64::from(device.logical_block_size());
        let mut records = Vec::new();
        let mut position = 0;
        while position + block <= area_len {
            let mut first = vec![0; block as usize];
            area.read_at(device, position, &mut first)?;
            let Some(header) = Header::decode(&first) else {
                break;
            };
            if header.guard_id != guard_id || header.seq != records.len() as u64 {
                break;
            }
            let record_len = padded_len(header.len as usize, block);
            if position + record_len > area_len {
                break;
            }
            let mut bytes = vec![0; record_len as usize];
            area.read_at(device, position, &mut bytes)?;
            let body_end = HEADER_LEN + header.len as usize;
            let stored = &bytes[body_end..body_end + CHECKSUM_LEN];
            if crc32c::crc32c(&bytes[..body_end]).to_le_bytes() != stored {
                break;
            }
            records.push(UndoRecord {
                offset: header.offset,
                original: bytes[HEADER_LEN..body_end].to_vec(),
            });
            position += record_len;
        }
        Ok(records)
    }
}

struct Header {
    guard_id: u64,
    seq: u64,
    offset: u64,
    len: u32,
}

impl Header {
    fn decode(bytes: &[u8]) -> Option<Header> {
        let field = |range: std::ops::Range<usize>| bytes.get(range);
        if field(0..8)? != MAGIC || u32::from_le_bytes(field(8..12)?.try_into().ok()?) != VERSION {
            return None;
        }
        Some(Header {
            guard_id: u64::from_le_bytes(field(12..20)?.try_into().ok()?),
            seq: u64::from_le_bytes(field(20..28)?.try_into().ok()?),
            offset: u64::from_le_bytes(field(28..36)?.try_into().ok()?),
            len: u32::from_le_bytes(field(36..40)?.try_into().ok()?),
        })
    }
}

fn encode(guard_id: u64, seq: u64, offset: u64, original: &[u8], block: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(padded_len(original.len(), block) as usize);
    out.extend(MAGIC);
    out.extend(VERSION.to_le_bytes());
    out.extend(guard_id.to_le_bytes());
    out.extend(seq.to_le_bytes());
    out.extend(offset.to_le_bytes());
    out.extend((original.len() as u32).to_le_bytes());
    out.extend(original);
    out.extend(crc32c::crc32c(&out).to_le_bytes());
    out.resize(padded_len(original.len(), block) as usize, 0);
    out
}

/// A record's size on disk: header, data and checksum, rounded up to whole blocks.
fn padded_len(data_len: usize, block: u64) -> u64 {
    ((HEADER_LEN + data_len + CHECKSUM_LEN) as u64).next_multiple_of(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal_store::FileExtent;
    use crate::{CrashDisk, MemDisk, PendingWrites};

    const AREA_AT: u64 = 1 << 20;
    const AREA_LEN: u64 = 1 << 20;

    fn area() -> JournalFile {
        JournalFile::covering(
            vec![FileExtent {
                file_offset: 0,
                device_offset: AREA_AT,
                len: AREA_LEN,
            }],
            AREA_LEN,
        )
        .unwrap()
    }

    fn disk() -> MemDisk {
        MemDisk::zeroed(4 << 20, 512).unwrap()
    }

    #[test]
    fn records_read_back_in_order() {
        let mut disk = disk();
        let mut log = UndoLog::create(area(), AREA_LEN, 7);
        log.append(&mut disk, 4096, &[1; 512]).unwrap();
        log.append(&mut disk, 8192, &[2; 1024]).unwrap();

        let records = UndoLog::read_all(&disk, &area(), AREA_LEN, 7).unwrap();

        assert_eq!(
            records,
            [
                UndoRecord {
                    offset: 4096,
                    original: vec![1; 512]
                },
                UndoRecord {
                    offset: 8192,
                    original: vec![2; 1024]
                },
            ]
        );
    }

    #[test]
    fn records_of_another_guarded_run_are_not_read() {
        let mut disk = disk();
        UndoLog::create(area(), AREA_LEN, 7)
            .append(&mut disk, 0, &[1; 512])
            .unwrap();
        assert!(
            UndoLog::read_all(&disk, &area(), AREA_LEN, 8)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_torn_record_ends_the_log() {
        let mut disk = disk();
        let mut log = UndoLog::create(area(), AREA_LEN, 7);
        log.append(&mut disk, 0, &[1; 512]).unwrap();
        disk.flush().unwrap();
        let mut crash = CrashDisk::new(disk);
        log.append(&mut crash, 512, &[2; 4096]).unwrap();

        for seed in 0..32 {
            let image = crash.crash_image(1, PendingWrites::Scrambled { seed });
            let records = UndoLog::read_all(&image, &area(), AREA_LEN, 7).unwrap();
            assert!(records.len() <= 2, "seed {seed}");
            assert_eq!(records[0].original, vec![1; 512], "seed {seed}");
            if let Some(second) = records.get(1) {
                assert_eq!(second.original, vec![2; 4096], "seed {seed}");
            }
        }
    }

    #[test]
    fn a_full_area_is_refused() {
        let mut disk = disk();
        let mut log = UndoLog::create(area(), 4096, 7);
        assert!(log.append(&mut disk, 0, &[1; 2048]).is_ok());
        assert!(matches!(
            log.append(&mut disk, 0, &[1; 2048]),
            Err(UndoError::Full(4096))
        ));
    }
}
