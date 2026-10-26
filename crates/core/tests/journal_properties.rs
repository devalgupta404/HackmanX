//! The journal decoders read bytes a power cut may have damaged, so they must never panic.

use bootshift_core::journal::{PlanRecord, Progress, Slot, Stage};
use proptest::prelude::*;

fn stage() -> impl Strategy<Value = Stage> {
    prop_oneof![
        Just(Stage::Prepared),
        Just(Stage::Running),
        Just(Stage::Verifying),
        Just(Stage::Finishing),
        Just(Stage::Done),
    ]
}

fn slot() -> impl Strategy<Value = Slot> {
    (
        any::<u64>(),
        any::<u64>(),
        stage(),
        any::<u32>(),
        any::<u8>(),
        any::<u64>(),
        any::<[u8; 32]>(),
    )
        .prop_map(
            |(seq, run_id, stage, operation, steps_done, next_chunk, hash_chain)| Slot {
                seq,
                run_id,
                progress: Progress {
                    stage,
                    operation,
                    steps_done,
                    next_chunk,
                    hash_chain,
                },
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5_000))]

    #[test]
    fn any_slot_round_trips(slot in slot()) {
        prop_assert_eq!(Slot::decode(&slot.encode()), Ok(slot));
    }

    #[test]
    fn any_plan_round_trips(run_id in any::<u64>(), payload in prop::collection::vec(any::<u8>(), 0..2048)) {
        let record = PlanRecord { run_id, payload };
        prop_assert_eq!(PlanRecord::decode(&record.encode()), Ok(record));
    }

    #[test]
    fn decoding_random_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..5000)) {
        let _ = Slot::decode(&bytes);
        let _ = PlanRecord::decode_header(&bytes);
        let _ = PlanRecord::decode(&bytes);
    }

    #[test]
    fn decoding_a_damaged_slot_never_panics(slot in slot(), index in 0usize..96, value in any::<u8>()) {
        let mut bytes = slot.encode();
        bytes[index] = value;
        let _ = Slot::decode(&bytes);
    }
}
