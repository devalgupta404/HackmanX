//! A small disk with a fresh journal and a data area, for copier tests.

#![allow(clippy::unwrap_used, dead_code)]

use std::ops::Range;
use std::sync::OnceLock;

use bootshift_core::journal::{self, PlanRecord, Progress, Stage};
use bootshift_disk::{BlockDevice, FileExtent, Journal, JournalFile, MemDisk};
use bootshift_engine::copier::{CopyOptions, CopyScope, MoveJob, move_range};

pub const BLOCK: u64 = 512;
pub const DATA_START: u64 = journal::FILE_LEN;
pub const DATA_BLOCKS: u64 = 96;

pub fn journal_file() -> JournalFile {
    JournalFile::new(vec![FileExtent {
        file_offset: 0,
        device_offset: 0,
        len: journal::FILE_LEN,
    }])
    .unwrap()
}

/// A disk with a fresh journal and a data area where every byte depends on its offset.
pub fn fresh_disk() -> MemDisk {
    static FRESH: OnceLock<MemDisk> = OnceLock::new();
    FRESH.get_or_init(build_fresh_disk).clone()
}

fn build_fresh_disk() -> MemDisk {
    let len = DATA_START + DATA_BLOCKS * BLOCK;
    let mut bytes: Vec<u8> = (0..len)
        .map(|offset| (offset.wrapping_mul(131) ^ (offset >> 9)) as u8)
        .collect();
    let plan = PlanRecord {
        run_id: 7,
        payload: Vec::new(),
    };
    let contents = journal::initial_file_contents(&plan);
    bytes[..contents.len()].copy_from_slice(&contents);
    MemDisk::from_bytes(bytes, BLOCK as u32).unwrap()
}

pub fn start_progress() -> Progress {
    Progress {
        stage: Stage::Running,
        operation: 0,
        steps_done: 0,
        next_chunk: 0,
        hash_chain: [0; 32],
    }
}

/// A move within the data area, with positions and lengths in blocks.
pub fn job(source: u64, destination: u64, len: u64, scope: CopyScope) -> MoveJob {
    MoveJob {
        source: DATA_START + source * BLOCK,
        destination: DATA_START + destination * BLOCK,
        len: len * BLOCK,
        scope,
    }
}

/// Opens the journal and runs the move from wherever it got to.
pub fn resume_to_end<D: BlockDevice>(
    disk: &mut D,
    job: &MoveJob,
    options: &CopyOptions,
) -> [u8; 32] {
    let opened = Journal::open(disk, journal_file()).unwrap();
    let start = opened.progress.unwrap_or_else(start_progress);
    let mut journal = opened.journal;
    move_range(disk, &mut journal, job.clone(), &start, options.clone())
        .unwrap()
        .hash_chain
}

fn copied_ranges(job: &MoveJob) -> Vec<Range<u64>> {
    match &job.scope {
        CopyScope::Everything => std::iter::once(0..job.len).collect(),
        CopyScope::UsedOnly(ranges) => ranges.clone(),
    }
}

pub fn destination_holds_source(original: &MemDisk, moved: &MemDisk, job: &MoveJob) -> bool {
    copied_ranges(job).into_iter().all(|range| {
        let source = (job.source + range.start) as usize..(job.source + range.end) as usize;
        let destination =
            (job.destination + range.start) as usize..(job.destination + range.end) as usize;
        moved.bytes()[destination] == original.bytes()[source]
    })
}
