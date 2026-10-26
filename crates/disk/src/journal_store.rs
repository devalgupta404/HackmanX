use std::ops::Range;

use bootshift_core::journal::{
    self, DecodeError, PLAN_OFFSET, PlanRecord, Progress, SCOPE_OFFSET, SLOT_OFFSETS, SLOT_SIZE,
    ScopeRecord, Slot,
};
use bootshift_core::scope::CopyScope;

use crate::device::{BlockDevice, DiskError};

/// One piece of the journal file on the device, as reported by FIEMAP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileExtent {
    pub file_offset: u64,
    pub device_offset: u64,
    pub len: u64,
}

/// Where the journal file lives on the device. The ESP stays unmounted during a move, so the
/// file is read and written through its extents, never through the FAT filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalFile {
    extents: Vec<FileExtent>,
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error(transparent)]
    Disk(#[from] DiskError),
    #[error("the journal file's extents don't cover the whole journal")]
    BadExtents,
    #[error("the journal's plan can't be read: {0}")]
    DamagedPlan(#[from] DecodeError),
    #[error("the journal file no longer holds this run's plan, so nothing was written")]
    NotOurs,
    #[error("the journal has no recorded scope for operation {0}")]
    NoScope(u32),
}

impl JournalFile {
    pub fn new(extents: Vec<FileExtent>) -> Result<Self, JournalError> {
        Self::covering(extents, journal::FILE_LEN)
    }

    /// Extents that must cover file offsets `0..len` with no holes.
    pub fn covering(mut extents: Vec<FileExtent>, len: u64) -> Result<Self, JournalError> {
        extents.sort_by_key(|extent| extent.file_offset);
        let mut covered: u64 = 0;
        for extent in &extents {
            if extent.file_offset != covered || extent.len == 0 {
                return Err(JournalError::BadExtents);
            }
            covered = covered
                .checked_add(extent.len)
                .ok_or(JournalError::BadExtents)?;
        }
        if covered < len {
            return Err(JournalError::BadExtents);
        }
        Ok(Self { extents })
    }

    /// Whether any extent touches the device byte range `range`.
    pub fn overlaps(&self, range: &Range<u64>) -> bool {
        self.extents.iter().any(|extent| {
            extent.device_offset < range.end && range.start < extent.device_offset + extent.len
        })
    }

    pub fn read_at<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        file_offset: u64,
        buf: &mut [u8],
    ) -> Result<(), JournalError> {
        for (device_offset, range) in self.pieces(file_offset, buf.len())? {
            device.read_at(device_offset, &mut buf[range])?;
        }
        Ok(())
    }

    pub fn write_at<D: BlockDevice + ?Sized>(
        &self,
        device: &mut D,
        file_offset: u64,
        data: &[u8],
    ) -> Result<(), JournalError> {
        for (device_offset, range) in self.pieces(file_offset, data.len())? {
            device.write_at(device_offset, &data[range])?;
        }
        Ok(())
    }

    /// Splits a file range into device offsets and the matching ranges of the caller's buffer.
    fn pieces(
        &self,
        file_offset: u64,
        len: usize,
    ) -> Result<Vec<(u64, Range<usize>)>, JournalError> {
        let end = file_offset
            .checked_add(len as u64)
            .ok_or(JournalError::BadExtents)?;
        let mut pieces = Vec::new();
        let mut covered = 0;
        for extent in &self.extents {
            let extent_end = extent.file_offset + extent.len;
            let start = file_offset.max(extent.file_offset);
            let stop = end.min(extent_end);
            if start < stop {
                let buf_start = (start - file_offset) as usize;
                let buf_end = (stop - file_offset) as usize;
                pieces.push((
                    extent.device_offset + (start - extent.file_offset),
                    buf_start..buf_end,
                ));
                covered += buf_end - buf_start;
            }
        }
        if covered == len {
            Ok(pieces)
        } else {
            Err(JournalError::BadExtents)
        }
    }

    fn read_plan_header<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
    ) -> Result<(u64, usize), JournalError> {
        let mut header = vec![0; SLOT_SIZE];
        self.read_at(device, PLAN_OFFSET, &mut header)?;
        Ok(PlanRecord::decode_header(&header)?)
    }

    fn read_plan<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<PlanRecord, JournalError> {
        let (_, record_len) = self.read_plan_header(device)?;
        let mut record = vec![0; record_len.next_multiple_of(SLOT_SIZE)];
        self.read_at(device, PLAN_OFFSET, &mut record)?;
        Ok(PlanRecord::decode(&record)?)
    }

    fn read_slot<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<Option<Slot>, JournalError> {
        let mut bytes = vec![0; SLOT_SIZE];
        self.read_at(device, offset, &mut bytes)?;
        Ok(Slot::decode(&bytes).ok())
    }
}

/// Commits progress for one run, alternating between the two slots.
pub struct Journal {
    file: JournalFile,
    run_id: u64,
    next_seq: u64,
}

pub struct OpenedJournal {
    pub journal: Journal,
    pub plan: PlanRecord,
    /// The newest progress committed for this run, if any.
    pub progress: Option<Progress>,
}

impl Journal {
    pub fn open<D: BlockDevice + ?Sized>(
        device: &D,
        file: JournalFile,
    ) -> Result<OpenedJournal, JournalError> {
        let plan = file.read_plan(device)?;
        let mut newest: Option<Slot> = None;
        for offset in SLOT_OFFSETS {
            let Some(slot) = file.read_slot(device, offset)? else {
                continue;
            };
            let is_newer = newest.as_ref().is_none_or(|n| slot.seq > n.seq);
            if slot.run_id == plan.run_id && is_newer {
                newest = Some(slot);
            }
        }

        let next_seq = newest.as_ref().map_or(0, |slot| slot.seq + 1);
        Ok(OpenedJournal {
            journal: Journal {
                file,
                run_id: plan.run_id,
                next_seq,
            },
            plan,
            progress: newest.map(|slot| slot.progress),
        })
    }

    /// Writes `progress` to the older slot and flushes. When this returns, the progress is
    /// durable; a power cut before then leaves the previous progress readable.
    pub fn commit<D: BlockDevice + ?Sized>(
        &mut self,
        device: &mut D,
        progress: &Progress,
    ) -> Result<(), JournalError> {
        self.check_still_ours(device)?;
        let slot = Slot {
            seq: self.next_seq,
            run_id: self.run_id,
            progress: progress.clone(),
        };
        let offset = SLOT_OFFSETS[(slot.seq % 2) as usize];
        self.file.write_at(device, offset, &slot.encode())?;
        device.flush()?;
        self.next_seq += 1;
        Ok(())
    }

    /// Records the used ranges of `operation` and flushes. Done before the move's first chunk
    /// is copied, because afterwards the source can no longer be measured.
    pub fn record_scope<D: BlockDevice + ?Sized>(
        &mut self,
        device: &mut D,
        operation: u32,
        scope: &CopyScope,
    ) -> Result<(), JournalError> {
        self.check_still_ours(device)?;
        let mut record = ScopeRecord {
            run_id: self.run_id,
            operation,
            scope: scope.clone(),
        }
        .encode();
        record.resize(record.len().next_multiple_of(SLOT_SIZE), 0);
        self.file.write_at(device, SCOPE_OFFSET, &record)?;
        device.flush()?;
        Ok(())
    }

    /// The used ranges recorded for `operation` of this run.
    pub fn recorded_scope<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        operation: u32,
    ) -> Result<CopyScope, JournalError> {
        let mut region = vec![0; (journal::FILE_LEN - SCOPE_OFFSET) as usize];
        self.file.read_at(device, SCOPE_OFFSET, &mut region)?;
        match ScopeRecord::decode(&region) {
            Ok(record) if record.run_id == self.run_id && record.operation == operation => {
                Ok(record.scope)
            }
            _ => Err(JournalError::NoScope(operation)),
        }
    }

    /// Fails unless the journal file still holds this run's plan, so a journal file that was
    /// moved or replaced is never written through stale extents.
    fn check_still_ours<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<(), JournalError> {
        let (run_id, _) = self.file.read_plan_header(device)?;
        if run_id == self.run_id {
            Ok(())
        } else {
            Err(JournalError::NotOurs)
        }
    }
}

#[cfg(test)]
mod tests {
    use bootshift_core::journal::{FILE_LEN, Stage, initial_file_contents};

    use super::*;
    use crate::{CrashDisk, MemDisk, PendingWrites};

    const MIB: u64 = 1 << 20;
    const RUN_ID: u64 = 0x5EED;

    /// The journal file in two pieces, stored out of order on the device.
    fn fragmented_file() -> JournalFile {
        JournalFile::new(vec![
            FileExtent {
                file_offset: 0,
                device_offset: 5 * MIB,
                len: 2 * MIB,
            },
            FileExtent {
                file_offset: 2 * MIB,
                device_offset: MIB,
                len: 2 * MIB,
            },
        ])
        .unwrap()
    }

    fn disk_with_fresh_journal() -> MemDisk {
        let mut disk = MemDisk::zeroed(8 * MIB as usize, 512).unwrap();
        let plan = PlanRecord {
            run_id: RUN_ID,
            payload: b"plan".to_vec(),
        };
        fragmented_file()
            .write_at(&mut disk, 0, &initial_file_contents(&plan))
            .unwrap();
        disk
    }

    fn progress(next_chunk: u64) -> Progress {
        Progress {
            stage: Stage::Running,
            operation: 0,
            steps_done: 0,
            next_chunk,
            hash_chain: [next_chunk as u8; 32],
        }
    }

    fn open(disk: &MemDisk) -> OpenedJournal {
        Journal::open(disk, fragmented_file()).unwrap()
    }

    #[test]
    fn fresh_journal_has_the_plan_and_no_progress() {
        let opened = open(&disk_with_fresh_journal());
        assert_eq!(opened.plan.payload, b"plan");
        assert_eq!(opened.progress, None);
    }

    #[test]
    fn reopening_returns_the_last_committed_progress() {
        let mut disk = disk_with_fresh_journal();
        let mut journal = open(&disk).journal;
        for chunk in 1..=3 {
            journal.commit(&mut disk, &progress(chunk)).unwrap();
        }
        assert_eq!(open(&disk).progress, Some(progress(3)));
    }

    #[test]
    fn every_crash_during_a_commit_leaves_the_old_or_new_progress() {
        let mut disk = disk_with_fresh_journal();
        open(&disk).journal.commit(&mut disk, &progress(1)).unwrap();

        let mut crash_disk = CrashDisk::new(disk);
        let mut journal = Journal::open(&crash_disk, fragmented_file())
            .unwrap()
            .journal;
        journal.commit(&mut crash_disk, &progress(2)).unwrap();

        let last_point = *crash_disk.crash_points().end();
        for point in crash_disk.crash_points() {
            let outcomes = [PendingWrites::Lost, PendingWrites::AllLanded]
                .into_iter()
                .chain((0..64).map(|seed| PendingWrites::Scrambled { seed }));
            for outcome in outcomes {
                let recovered = open(&crash_disk.crash_image(point, outcome)).progress;
                if point == last_point {
                    assert_eq!(recovered, Some(progress(2)), "{outcome:?}");
                } else {
                    assert!(
                        recovered == Some(progress(1)) || recovered == Some(progress(2)),
                        "point {point}, {outcome:?}: {recovered:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn commits_continue_after_a_torn_slot_write() {
        let mut disk = disk_with_fresh_journal();
        open(&disk).journal.commit(&mut disk, &progress(1)).unwrap();
        let mut crash_disk = CrashDisk::new(disk);
        let mut journal = Journal::open(&crash_disk, fragmented_file())
            .unwrap()
            .journal;
        journal.commit(&mut crash_disk, &progress(2)).unwrap();

        for seed in 0..32 {
            let mut recovered = crash_disk.crash_image(1, PendingWrites::Scrambled { seed });
            let mut journal = open(&recovered).journal;
            journal.commit(&mut recovered, &progress(3)).unwrap();
            assert_eq!(open(&recovered).progress, Some(progress(3)), "seed {seed}");
        }
    }

    #[test]
    fn slots_from_another_run_are_ignored() {
        let mut disk = disk_with_fresh_journal();
        let stale = Slot {
            seq: 99,
            run_id: RUN_ID + 1,
            progress: progress(7),
        };
        fragmented_file()
            .write_at(&mut disk, SLOT_OFFSETS[1], &stale.encode())
            .unwrap();

        assert_eq!(open(&disk).progress, None);
    }

    #[test]
    fn refuses_to_write_once_the_plan_record_is_gone() {
        let mut disk = disk_with_fresh_journal();
        let mut journal = open(&disk).journal;
        fragmented_file()
            .write_at(&mut disk, PLAN_OFFSET, &[0; SLOT_SIZE])
            .unwrap();
        let before = disk.clone();

        let result = journal.commit(&mut disk, &progress(1));

        assert!(matches!(result, Err(JournalError::DamagedPlan(_))));
        assert!(disk == before);
    }

    #[test]
    fn refuses_to_write_when_the_plan_belongs_to_another_run() {
        let mut disk = disk_with_fresh_journal();
        let mut journal = open(&disk).journal;
        let other = PlanRecord {
            run_id: RUN_ID + 1,
            payload: Vec::new(),
        };
        fragmented_file()
            .write_at(&mut disk, 0, &initial_file_contents(&other))
            .unwrap();

        assert!(matches!(
            journal.commit(&mut disk, &progress(1)),
            Err(JournalError::NotOurs)
        ));
    }

    #[test]
    fn recorded_scope_reads_back_for_its_operation_only() {
        let mut disk = disk_with_fresh_journal();
        let mut journal = open(&disk).journal;
        let scope = CopyScope::UsedOnly(vec![0..4096, 8192..16384]);

        journal.record_scope(&mut disk, 3, &scope).unwrap();

        assert_eq!(journal.recorded_scope(&disk, 3).unwrap(), scope);
        assert!(matches!(
            journal.recorded_scope(&disk, 4),
            Err(JournalError::NoScope(4))
        ));
    }

    #[test]
    fn a_fresh_journal_has_no_scope() {
        let disk = disk_with_fresh_journal();
        assert!(open(&disk).journal.recorded_scope(&disk, 0).is_err());
    }

    #[test]
    fn reads_and_writes_span_extent_boundaries() {
        let mut disk = MemDisk::zeroed(8 * MIB as usize, 512).unwrap();
        let file = fragmented_file();
        let data: Vec<u8> = (0..4096u32).map(|i| i as u8).collect();

        file.write_at(&mut disk, 2 * MIB - 2048, &data).unwrap();

        let mut back = vec![0; 4096];
        file.read_at(&disk, 2 * MIB - 2048, &mut back).unwrap();
        assert_eq!(back, data);
        let tail_on_device = &disk.bytes()[MIB as usize..MIB as usize + 2048];
        assert_eq!(tail_on_device, &data[2048..]);
    }

    #[test]
    fn rejects_extents_with_a_hole_or_too_short() {
        let hole = vec![
            FileExtent {
                file_offset: 0,
                device_offset: 0,
                len: MIB,
            },
            FileExtent {
                file_offset: 2 * MIB,
                device_offset: MIB,
                len: 2 * MIB,
            },
        ];
        let short = vec![FileExtent {
            file_offset: 0,
            device_offset: 0,
            len: FILE_LEN - 512,
        }];

        assert!(JournalFile::new(hole).is_err());
        assert!(JournalFile::new(short).is_err());
    }
}
