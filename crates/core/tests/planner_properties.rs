//! Random layouts and targets, checked by replaying every plan step by step.

use bootshift_core::layout::{Content, Extent, Geometry, Guid, Layout, Partition, Target};
use bootshift_core::plan::{Operation, Plan};
use bootshift_core::planner::{PlanError, plan};
use bootshift_core::units::{Lba, Sectors};
use proptest::prelude::*;

const SECTORS_PER_MIB: u64 = 2048;
const MAX_GROWTH_MIB: u64 = 10;
const MAX_GAP_MIB: u64 = 5;

fn mib(n: u64) -> Sectors {
    Sectors(n * SECTORS_PER_MIB)
}

#[derive(Clone, Debug)]
struct PartitionSpec {
    gap_mib: u64,
    len_mib: u64,
    content: Content,
    used_percent: u64,
}

#[derive(Clone, Debug)]
struct Case {
    current: Layout,
    target: Target,
}

fn content() -> impl Strategy<Value = Content> {
    prop_oneof![
        Just(Content::Ext4),
        Just(Content::Ntfs),
        Just(Content::Btrfs),
        Just(Content::Xfs),
        Just(Content::Swap),
        Just(Content::Opaque),
    ]
}

fn partition_specs() -> impl Strategy<Value = Vec<PartitionSpec>> {
    prop::collection::vec(
        (0..=MAX_GAP_MIB, 1u64..20, content(), 0u64..=100).prop_map(
            |(gap_mib, len_mib, content, used_percent)| PartitionSpec {
                gap_mib,
                len_mib,
                content,
                used_percent,
            },
        ),
        1..6,
    )
}

/// A disk big enough to hold any target built from these specs, with partitions packed from
/// 1 MiB in the given order.
fn current_layout(specs: &[PartitionSpec]) -> Layout {
    let capacity_mib: u64 = specs
        .iter()
        .map(|s| s.len_mib + MAX_GROWTH_MIB + 2 * MAX_GAP_MIB)
        .sum();
    let mut cursor = 1;
    let partitions = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            cursor += spec.gap_mib;
            let len = mib(spec.len_mib);
            let used = Sectors(len.0 * spec.used_percent / 100);
            let partition = Partition {
                number: i as u32 + 1,
                unique_guid: Guid([i as u8 + 1; 16]),
                type_guid: Guid([0; 16]),
                name: String::new(),
                attributes: 0,
                extent: Extent::new(Lba(cursor * SECTORS_PER_MIB), len),
                content: spec.content,
                used,
                min_len: Some(Sectors(used.0.div_ceil(SECTORS_PER_MIB) * SECTORS_PER_MIB)),
            };
            cursor += spec.len_mib;
            partition
        })
        .collect();
    Layout {
        geometry: Geometry {
            sector_size: 512,
            usable: Extent::new(Lba(SECTORS_PER_MIB), mib(capacity_mib)),
            alignment: mib(1),
        },
        partitions,
    }
}

/// A target size each partition is allowed to take, clamped to what its contents support.
fn allowed_len_mib(partition: &Partition, delta: i64) -> u64 {
    let current = partition.extent.len.0 / SECTORS_PER_MIB;
    let min_mib = partition.min_len.map_or(current, |s| s.0 / SECTORS_PER_MIB);
    let lower = match partition.content {
        Content::Swap => 1,
        c if c.can_shrink() => min_mib.max(1),
        _ => current,
    };
    let upper = if partition.content.can_grow() {
        current + MAX_GROWTH_MIB
    } else {
        current
    };
    current.saturating_add_signed(delta).clamp(lower, upper)
}

fn target_layout(current: &Layout, order: &[usize], deltas: &[i64], gaps: &[u64]) -> Target {
    let mut cursor = 1;
    order
        .iter()
        .map(|&i| {
            let partition = &current.partitions[i];
            cursor += gaps[i];
            let len_mib = allowed_len_mib(partition, deltas[i]);
            let extent = Extent::new(Lba(cursor * SECTORS_PER_MIB), mib(len_mib));
            cursor += len_mib;
            (partition.unique_guid, extent)
        })
        .collect()
}

fn case(keep_order: bool) -> impl Strategy<Value = Case> {
    partition_specs()
        .prop_flat_map(|specs| {
            let n = specs.len();
            (
                Just(specs),
                prop::collection::vec(-10i64..10, n),
                prop::collection::vec(0..=MAX_GAP_MIB, n),
                Just((0..n).collect::<Vec<_>>()).prop_shuffle(),
            )
        })
        .prop_map(move |(specs, deltas, gaps, shuffled)| {
            let current = current_layout(&specs);
            let order: Vec<usize> = if keep_order {
                (0..specs.len()).collect()
            } else {
                shuffled
            };
            let target = target_layout(&current, &order, &deltas, &gaps);
            Case { current, target }
        })
}

fn overlapping_pair(layout: &Layout) -> Option<(u32, u32)> {
    let parts = &layout.partitions;
    parts.iter().enumerate().find_map(|(i, a)| {
        parts[i + 1..]
            .iter()
            .find(|b| a.extent.overlaps(b.extent))
            .map(|b| (a.number, b.number))
    })
}

fn check_plan_is_safe(case: &Case, plan: &Plan) -> Result<(), TestCaseError> {
    let mut layout = case.current.clone();
    for (step, operation) in plan.operations.iter().enumerate() {
        if let Operation::Move { from, to, .. } = operation {
            prop_assert_ne!(from, to, "step {} moves a partition nowhere", step);
        }
        layout
            .apply(operation)
            .map_err(|e| TestCaseError::fail(format!("step {step} does not apply: {e}")))?;
        prop_assert_eq!(
            overlapping_pair(&layout),
            None,
            "overlap after step {}",
            step
        );
        for partition in &layout.partitions {
            prop_assert!(
                layout.geometry.usable.contains(partition.extent),
                "partition {} leaves the disk after step {}",
                partition.number,
                step
            );
        }
    }

    prop_assert_eq!(layout.partitions.len(), case.current.partitions.len());
    for partition in &layout.partitions {
        prop_assert_eq!(
            Some(&partition.extent),
            case.target.get(&partition.unique_guid),
            "partition {} did not reach its target",
            partition.number
        );
    }
    Ok(())
}

/// Copy cost when each moved partition is copied exactly once, at its smaller size.
fn copy_when_each_partition_moves_once(case: &Case) -> (usize, Sectors) {
    let mut moves = 0;
    let mut copied = Sectors(0);
    for partition in &case.current.partitions {
        let target = case.target[&partition.unique_guid];
        if partition.content == Content::Swap || target.start == partition.extent.start {
            continue;
        }
        let len = partition.extent.len.min(target.len);
        moves += 1;
        copied += if partition.content.copies_used_only() {
            partition.used.min(len)
        } else {
            len
        };
    }
    (moves, copied)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn any_plan_reaches_the_target_without_overlap(case in case(false)) {
        match plan(&case.current, &case.target) {
            Ok(plan) => check_plan_is_safe(&case, &plan)?,
            Err(PlanError::Refused(_)) => {}
            Err(PlanError::Internal(e)) => prop_assert!(false, "planner bug: {}", e),
        }
    }

    #[test]
    fn order_preserving_targets_always_plan_and_copy_each_partition_once(case in case(true)) {
        let plan = plan(&case.current, &case.target)
            .map_err(|e| TestCaseError::fail(format!("refused a reachable layout: {e}")))?;
        check_plan_is_safe(&case, &plan)?;

        let (expected_moves, expected_copy) = copy_when_each_partition_moves_once(&case);
        let moves = plan
            .operations
            .iter()
            .filter(|op| matches!(op, Operation::Move { .. }))
            .count();
        prop_assert_eq!(moves, expected_moves);
        prop_assert_eq!(plan.sectors_to_copy(), expected_copy);
    }
}
