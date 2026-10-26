//! Every move is replayed with a power cut at every write and flush, then resumed from the
//! journal. The destination must always end up holding exactly the source's data.

#![allow(clippy::unwrap_used)]

mod common;

use bootshift_disk::{CrashDisk, FaultyDisk, Journal, MemDisk, PendingWrites};
use bootshift_engine::copier::{
    ChunkCopier, CopyError, CopyOptions, CopyScope, MoveJob, move_range, verify_move,
};
use common::{
    BLOCK, destination_holds_source, fresh_disk, job, journal_file, resume_to_end, start_progress,
};

fn crash_outcomes() -> impl Iterator<Item = PendingWrites> {
    [PendingWrites::Lost, PendingWrites::AllLanded]
        .into_iter()
        .chain((0..12).map(|seed| PendingWrites::Scrambled { seed }))
}

fn assert_survives_every_crash(job: MoveJob, options: CopyOptions) {
    let original = fresh_disk();
    let mut recorded = CrashDisk::new(original.clone());
    resume_to_end(&mut recorded, &job, &options);

    for point in recorded.crash_points() {
        for outcome in crash_outcomes() {
            let context = format!("crash at point {point} with {outcome:?}");
            let mut image = recorded.crash_image(point, outcome);
            let chain = resume_to_end(&mut image, &job, &options);
            assert!(
                destination_holds_source(&original, &image, &job),
                "{context}: data was not moved intact"
            );
            assert_eq!(
                verify_move(&image, job.clone(), &options).unwrap(),
                chain,
                "{context}: hash chain does not verify"
            );
        }
    }
}

#[test]
fn move_left_by_one_block_survives_every_crash() {
    assert_survives_every_crash(job(10, 9, 6, CopyScope::Everything), CopyOptions::default());
}

#[test]
fn overlapping_move_left_with_a_partial_last_chunk_survives_every_crash() {
    assert_survives_every_crash(
        job(10, 7, 10, CopyScope::Everything),
        CopyOptions::default(),
    );
}

#[test]
fn move_right_by_one_block_survives_every_crash() {
    assert_survives_every_crash(
        job(10, 11, 6, CopyScope::Everything),
        CopyOptions::default(),
    );
}

#[test]
fn overlapping_move_right_with_a_partial_last_chunk_survives_every_crash() {
    assert_survives_every_crash(
        job(10, 13, 10, CopyScope::Everything),
        CopyOptions::default(),
    );
}

#[test]
fn move_into_adjacent_space_survives_every_crash() {
    assert_survives_every_crash(
        job(20, 10, 10, CopyScope::Everything),
        CopyOptions::default(),
    );
}

#[test]
fn move_across_a_gap_survives_every_crash() {
    assert_survives_every_crash(
        job(10, 40, 8, CopyScope::Everything),
        CopyOptions::default(),
    );
}

#[test]
fn move_with_chunks_shorter_than_the_distance_survives_every_crash() {
    let options = CopyOptions {
        max_chunk: 2 * BLOCK,
        ..CopyOptions::default()
    };
    assert_survives_every_crash(job(17, 10, 10, CopyScope::Everything), options);
}

#[test]
fn move_of_used_ranges_only_survives_every_crash() {
    let scope = CopyScope::UsedOnly(vec![BLOCK..3 * BLOCK, 7 * BLOCK + 100..9 * BLOCK]);
    assert_survives_every_crash(job(12, 10, 12, scope), CopyOptions::default());
}

#[test]
fn verify_detects_a_changed_byte_in_the_destination() {
    let job = job(10, 7, 10, CopyScope::Everything);
    let options = CopyOptions::default();
    let mut disk = fresh_disk();
    let chain = resume_to_end(&mut disk, &job, &options);

    let mut bytes = disk.into_bytes();
    bytes[(job.destination + 1000) as usize] ^= 1;
    let damaged = MemDisk::from_bytes(bytes, BLOCK as u32).unwrap();

    assert_ne!(verify_move(&damaged, job, &options).unwrap(), chain);
}

#[test]
fn refuses_to_copy_another_chunk_before_the_last_is_committed() {
    let mut disk = fresh_disk();
    let mut copier = ChunkCopier::resume(
        job(10, 9, 6, CopyScope::Everything),
        BLOCK as u32,
        0,
        [0; 32],
        CopyOptions::default(),
    )
    .unwrap();

    assert!(copier.copy_next_chunk(&mut disk).unwrap().is_some());
    assert!(matches!(
        copier.copy_next_chunk(&mut disk),
        Err(CopyError::UncommittedChunk(0))
    ));
}

#[test]
fn unreadable_source_block_stops_the_move_and_names_the_block() {
    let job = job(10, 7, 10, CopyScope::Everything);
    let bad_block = job.source + 5 * BLOCK;
    let mut disk = FaultyDisk::new(fresh_disk(), [bad_block]);
    let mut journal = Journal::open(&disk, journal_file()).unwrap().journal;

    let result = move_range(
        &mut disk,
        &mut journal,
        job,
        &start_progress(),
        CopyOptions::default(),
    );

    assert!(matches!(result, Err(CopyError::Unreadable { offset }) if offset == bad_block));
    let progress = Journal::open(&disk, journal_file())
        .unwrap()
        .progress
        .unwrap();
    assert_eq!(
        progress.next_chunk, 1,
        "only the chunk before the bad block is committed"
    );
}

#[test]
fn accepted_unreadable_block_is_written_as_zeros_and_reported() {
    let job = job(10, 7, 10, CopyScope::Everything);
    let bad_block = job.source + 5 * BLOCK;
    let mut disk = FaultyDisk::new(fresh_disk(), [bad_block]);
    let mut journal = Journal::open(&disk, journal_file()).unwrap().journal;
    let options = CopyOptions {
        accept_unreadable: true,
        ..CopyOptions::default()
    };

    let outcome = move_range(
        &mut disk,
        &mut journal,
        job.clone(),
        &start_progress(),
        options,
    )
    .unwrap();

    assert_eq!(outcome.unreadable, vec![bad_block]);
    let disk = disk.into_inner();
    let zeroed = (job.destination + 5 * BLOCK) as usize;
    assert!(
        disk.bytes()[zeroed..zeroed + BLOCK as usize]
            .iter()
            .all(|&b| b == 0)
    );
}

#[test]
fn unreadable_block_outside_the_used_ranges_is_never_read() {
    let scope = CopyScope::UsedOnly(vec![0..2 * BLOCK, 9 * BLOCK..10 * BLOCK]);
    let job = job(10, 7, 10, scope);
    let mut disk = FaultyDisk::new(fresh_disk(), [job.source + 8 * BLOCK]);
    let mut journal = Journal::open(&disk, journal_file()).unwrap().journal;

    let result = move_range(
        &mut disk,
        &mut journal,
        job,
        &start_progress(),
        CopyOptions::default(),
    );

    assert!(result.is_ok());
}
