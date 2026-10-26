//! Random moves, with a power cut during the move and a second one during recovery.

#![allow(clippy::unwrap_used)]

mod common;

use std::ops::RangeInclusive;

use bootshift_disk::{CrashDisk, PendingWrites};
use bootshift_engine::copier::{CopyOptions, CopyScope, MoveJob};
use common::{BLOCK, DATA_BLOCKS, destination_holds_source, fresh_disk, job, resume_to_end};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
struct Crash {
    /// Where in the run the power goes out, in thousandths of the way through.
    at_permille: u64,
    outcome: PendingWrites,
}

impl Crash {
    fn point(&self, points: RangeInclusive<usize>) -> usize {
        let last = *points.end() as u64;
        (last * self.at_permille / 1000) as usize
    }
}

fn crash() -> impl Strategy<Value = Crash> {
    let outcome = prop_oneof![
        Just(PendingWrites::Lost),
        Just(PendingWrites::AllLanded),
        any::<u64>().prop_map(|seed| PendingWrites::Scrambled { seed }),
    ];
    (0u64..=1000, outcome).prop_map(|(at_permille, outcome)| Crash {
        at_permille,
        outcome,
    })
}

fn scope(len_blocks: u64) -> impl Strategy<Value = CopyScope> {
    let len = len_blocks * BLOCK;
    prop_oneof![
        Just(CopyScope::Everything),
        prop::collection::vec((0..len, 1..4 * BLOCK), 0..4).prop_map(move |ranges| {
            let used = ranges
                .into_iter()
                .map(|(start, size)| start..(start + size).min(len));
            CopyScope::used_only(len, used)
        }),
    ]
}

fn move_job() -> impl Strategy<Value = (MoveJob, CopyOptions)> {
    (1u64..20, 1u64..=4)
        .prop_flat_map(|(len, max_chunk_blocks)| {
            let positions = 0..=DATA_BLOCKS - len;
            (
                positions.clone(),
                positions,
                Just(len),
                scope(len),
                Just(max_chunk_blocks),
            )
        })
        .prop_filter("a move must go somewhere", |(source, destination, ..)| {
            source != destination
        })
        .prop_map(|(source, destination, len, scope, max_chunk_blocks)| {
            let options = CopyOptions {
                max_chunk: max_chunk_blocks * BLOCK,
                ..CopyOptions::default()
            };
            (job(source, destination, len, scope), options)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn moves_survive_a_crash_during_recovery(
        (job, options) in move_job(),
        first in crash(),
        second in crash(),
    ) {
        let original = fresh_disk();
        let mut recorded = CrashDisk::new(original.clone());
        resume_to_end(&mut recorded, &job, &options);

        let first_image = recorded.crash_image(first.point(recorded.crash_points()), first.outcome);
        let mut recovering = CrashDisk::new(first_image);
        resume_to_end(&mut recovering, &job, &options);

        let mut second_image =
            recovering.crash_image(second.point(recovering.crash_points()), second.outcome);
        resume_to_end(&mut second_image, &job, &options);

        prop_assert!(destination_holds_source(&original, &second_image, &job));
    }
}
