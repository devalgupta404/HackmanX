//! Guarded runs: a window onto one partition that keeps an undo log, so a resize tool that
//! can't survive a power cut on its own can be put back exactly as it was and run again.
//!
//! Before the tool's first write to a block that held data, the block's original contents are
//! appended to the undo log and flushed; only then does the write go to the disk. Blocks that
//! were free don't need saving: once the filesystem's own metadata is restored, nothing refers
//! to them.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::device::{BlockDevice, DiskError, check_access};
use crate::journal_store::JournalFile;
use crate::undo_log::{UndoError, UndoLog};

pub struct GuardedDevice<'a, D: BlockDevice + ?Sized> {
    disk: &'a mut D,
    window: Range<u64>,
    /// Byte ranges of the window that held data when the run started.
    protected: Vec<Range<u64>>,
    saved: BTreeSet<u64>,
    log: UndoLog,
}

impl<'a, D: BlockDevice + ?Sized> GuardedDevice<'a, D> {
    /// Guards the partition at `window` (byte offsets on `disk`). The undo area must lie
    /// outside the window.
    pub fn new(
        disk: &'a mut D,
        window: Range<u64>,
        protected: Vec<Range<u64>>,
        undo_area: JournalFile,
        undo_area_len: u64,
        guard_id: u64,
    ) -> Result<Self, UndoError> {
        if undo_area.overlaps(&window) {
            return Err(UndoError::AreaInsideWindow);
        }
        Ok(Self {
            disk,
            window,
            protected,
            saved: BTreeSet::new(),
            log: UndoLog::create(undo_area, undo_area_len, guard_id),
        })
    }

    fn block_size(&self) -> u64 {
        u64::from(self.disk.logical_block_size())
    }

    /// Runs of not-yet-saved blocks holding original data within `range`, in window offsets.
    fn unsaved_runs(&self, range: Range<u64>) -> Vec<Range<u64>> {
        let block = self.block_size();
        let mut runs: Vec<Range<u64>> = Vec::new();
        let mut offset = range.start;
        while offset < range.end {
            let needs_saving = !self.saved.contains(&(offset / block))
                && self.protected.iter().any(|p| p.contains(&offset));
            if needs_saving {
                match runs.last_mut() {
                    Some(run) if run.end == offset => run.end += block,
                    _ => runs.push(offset..offset + block),
                }
            }
            offset += block;
        }
        runs
    }

    fn save_originals(&mut self, runs: &[Range<u64>]) -> Result<(), UndoError> {
        for run in runs {
            let mut original = vec![0; (run.end - run.start) as usize];
            self.disk
                .read_at(self.window.start + run.start, &mut original)?;
            self.log.append(self.disk, run.start, &original)?;
        }
        // The originals must be on disk before any write can overwrite them.
        self.disk.flush()?;
        let block = self.block_size();
        for run in runs {
            self.saved.extend((run.start / block)..(run.end / block));
        }
        Ok(())
    }
}

impl<D: BlockDevice + ?Sized> BlockDevice for GuardedDevice<'_, D> {
    fn len_bytes(&self) -> u64 {
        self.window.end - self.window.start
    }

    fn logical_block_size(&self) -> u32 {
        self.disk.logical_block_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        check_access(
            offset,
            buf.len(),
            self.len_bytes(),
            self.logical_block_size(),
        )?;
        self.disk.read_at(self.window.start + offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        let end = check_access(
            offset,
            data.len(),
            self.len_bytes(),
            self.logical_block_size(),
        )?;
        let runs = self.unsaved_runs(offset..end);
        if !runs.is_empty() {
            self.save_originals(&runs)
                .map_err(UndoError::into_disk_error)?;
        }
        self.disk.write_at(self.window.start + offset, data)
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.disk.flush()
    }
}

/// Puts back every block the guarded run `guard_id` saved, undoing the tool's changes. Safe to
/// repeat. Returns how many runs of blocks were restored.
pub fn roll_back<D: BlockDevice + ?Sized>(
    disk: &mut D,
    window_start: u64,
    undo_area: &JournalFile,
    undo_area_len: u64,
    guard_id: u64,
) -> Result<usize, UndoError> {
    let records = UndoLog::read_all(disk, undo_area, undo_area_len, guard_id)?;
    for record in records.iter().rev() {
        disk.write_at(window_start + record.offset, &record.original)?;
    }
    disk.flush()?;
    Ok(records.len())
}
