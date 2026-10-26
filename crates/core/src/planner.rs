use std::collections::BTreeSet;

use crate::layout::{Content, Extent, Guid, Layout, Partition, Target};
use crate::plan::{ApplyError, Operation, Plan};
use crate::refusal::Refusal;
use crate::units::{Lba, Sectors};
use crate::validate::validate;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error("planner bug: {0}")]
    Internal(#[from] ApplyError),
}

/// Works out the operations that turn `current` into `target` without two partitions ever
/// sharing space.
///
/// Swap partitions that change are dropped first and recreated last. Then partitions shrink
/// in place, move, and finally grow in place. Moves that keep partitions in order always
/// exist and copy each partition once; reordering may park a partition in free space first.
pub fn plan(current: &Layout, target: &Target) -> Result<Plan, PlanError> {
    validate(current, target)?;
    let mut planner = Planner {
        layout: current.clone(),
        target,
        operations: Vec::new(),
        parked: BTreeSet::new(),
    };

    let swaps = planner.remove_changing_swaps()?;
    planner.shrink()?;
    planner.move_all()?;
    planner.grow()?;
    for swap in swaps {
        planner.push(Operation::RecreateSwap { partition: swap })?;
    }
    Ok(Plan {
        operations: planner.operations,
    })
}

struct Planner<'a> {
    layout: Layout,
    target: &'a Target,
    operations: Vec<Operation>,
    parked: BTreeSet<Guid>,
}

impl Planner<'_> {
    fn push(&mut self, operation: Operation) -> Result<(), ApplyError> {
        self.layout.apply(&operation)?;
        self.operations.push(operation);
        Ok(())
    }

    fn target_of(&self, partition: &Partition) -> Result<Extent, ApplyError> {
        self.target
            .get(&partition.unique_guid)
            .copied()
            .ok_or(ApplyError::UnknownPartition(partition.unique_guid))
    }

    fn remove_changing_swaps(&mut self) -> Result<Vec<Partition>, ApplyError> {
        let mut removed = Vec::new();
        for partition in self.layout.partitions.clone() {
            let target = self.target_of(&partition)?;
            if partition.content == Content::Swap && partition.extent != target {
                self.push(Operation::RemoveSwap {
                    partition: partition.unique_guid,
                })?;
                removed.push(Partition {
                    extent: target,
                    ..partition
                });
            }
        }
        Ok(removed)
    }

    fn shrink(&mut self) -> Result<(), ApplyError> {
        for partition in self.layout.partitions.clone() {
            let target = self.target_of(&partition)?;
            if target.len < partition.extent.len {
                self.push(Operation::Shrink {
                    partition: partition.unique_guid,
                    len: target.len,
                })?;
            }
        }
        Ok(())
    }

    fn grow(&mut self) -> Result<(), ApplyError> {
        for partition in self.layout.partitions.clone() {
            let target = self.target_of(&partition)?;
            if target.len > partition.extent.len {
                self.push(Operation::Grow {
                    partition: partition.unique_guid,
                    len: target.len,
                })?;
            }
        }
        Ok(())
    }

    /// Each pass either moves a partition to its target, or parks a partition that is in the
    /// way somewhere no target needs. A parked partition blocks nobody, so it is parked at most
    /// once, and the loop ends.
    fn move_all(&mut self) -> Result<(), PlanError> {
        loop {
            if let Some((partition, to)) = self.next_move_to_target()? {
                self.push_move(&partition, to)?;
            } else if let Some((partition, to)) = self.next_parking_move()? {
                self.parked.insert(partition.unique_guid);
                self.push_move(&partition, to)?;
            } else if self.pending()?.is_empty() {
                return Ok(());
            } else {
                return Err(Refusal::new(
                    "There isn't enough free space to rearrange the partitions in this order.",
                    "Keep the partitions in their current order, or make room by shrinking one.",
                )
                .into());
            }
        }
    }

    fn push_move(&mut self, partition: &Partition, to: Lba) -> Result<(), ApplyError> {
        self.push(Operation::Move {
            partition: partition.unique_guid,
            from: partition.extent.start,
            to,
            len: partition.extent.len,
            copy_estimate: partition.sectors_to_copy(),
        })
    }

    fn pending(&self) -> Result<Vec<Partition>, ApplyError> {
        let mut pending = Vec::new();
        for partition in &self.layout.partitions {
            if self.target_of(partition)?.start != partition.extent.start {
                pending.push(partition.clone());
            }
        }
        Ok(pending)
    }

    /// The next partition whose target is free, moving Windows (NTFS) first so the window in
    /// which Windows could be booted against a half-moved disk stays short.
    fn next_move_to_target(&self) -> Result<Option<(Partition, Lba)>, ApplyError> {
        let mut ready = Vec::new();
        for partition in self.pending()? {
            let to = self.target_of(&partition)?.start;
            if self.is_free_for(&partition, Extent::new(to, partition.extent.len)) {
                ready.push((partition, to));
            }
        }
        Ok(ready
            .into_iter()
            .min_by_key(|(p, _)| (p.content != Content::Ntfs, p.extent.start)))
    }

    /// The cheapest partition that blocks another's target, with a spot to park it that no
    /// pending partition's target touches.
    fn next_parking_move(&self) -> Result<Option<(Partition, Lba)>, ApplyError> {
        let pending = self.pending()?;
        let mut pending_targets = Vec::new();
        for partition in &pending {
            pending_targets.push((partition.unique_guid, self.target_of(partition)?));
        }

        let mut blockers: Vec<&Partition> = pending
            .iter()
            .filter(|p| !self.parked.contains(&p.unique_guid))
            .filter(|p| {
                pending_targets
                    .iter()
                    .any(|(guid, target)| *guid != p.unique_guid && target.overlaps(p.extent))
            })
            .collect();
        blockers.sort_by_key(|p| (p.sectors_to_copy(), p.extent.start));

        for blocker in blockers {
            let mut occupied: Vec<Extent> = self
                .layout
                .partitions
                .iter()
                .filter(|p| p.unique_guid != blocker.unique_guid)
                .map(|p| p.extent)
                .collect();
            occupied.extend(pending_targets.iter().map(|(_, target)| *target));
            if let Some(to) = self.first_fit(&occupied, blocker.extent.len) {
                return Ok(Some((blocker.clone(), to)));
            }
        }
        Ok(None)
    }

    fn is_free_for(&self, partition: &Partition, destination: Extent) -> bool {
        self.layout
            .partitions
            .iter()
            .filter(|p| p.unique_guid != partition.unique_guid)
            .all(|p| !p.extent.overlaps(destination))
    }

    fn first_fit(&self, occupied: &[Extent], len: Sectors) -> Option<Lba> {
        let geometry = &self.layout.geometry;
        let mut occupied = occupied.to_vec();
        occupied.push(Extent::new(geometry.usable.end(), Sectors(0)));
        occupied.sort_by_key(|extent| extent.start);

        let mut gap_start = geometry.usable.start;
        for extent in occupied {
            let candidate = Extent::new(gap_start.align_up(geometry.alignment), len);
            if candidate.end() <= extent.start && geometry.usable.contains(candidate) {
                return Some(candidate.start);
            }
            gap_start = gap_start.max(extent.end());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn operations(current: &Layout, target: &Target) -> Vec<Operation> {
        plan(current, target).unwrap().operations
    }

    #[test]
    fn gives_windows_space_to_linux_past_the_recovery_partition() {
        let current = layout(
            1000,
            vec![
                partition(1, Content::Ntfs, 1, 400),
                partition(2, Content::Ntfs, 401, 1),
                partition(3, Content::Ext4, 402, 400),
            ],
        );
        let target = target_of(&current, &[(1, 1, 300), (2, 301, 1), (3, 302, 500)]);

        let ops = operations(&current, &target);

        assert_eq!(
            ops,
            vec![
                Operation::Shrink {
                    partition: guid(1),
                    len: mib(300)
                },
                Operation::Move {
                    partition: guid(2),
                    from: at(401),
                    to: at(301),
                    len: mib(1),
                    copy_estimate: Sectors(1024),
                },
                Operation::Move {
                    partition: guid(3),
                    from: at(402),
                    to: at(302),
                    len: mib(400),
                    copy_estimate: mib(200),
                },
                Operation::Grow {
                    partition: guid(3),
                    len: mib(500)
                },
            ]
        );
    }

    #[test]
    fn grows_in_place_without_copying() {
        let current = layout(100, vec![partition(1, Content::Ext4, 1, 40)]);
        let target = target_of(&current, &[(1, 1, 90)]);

        let ops = operations(&current, &target);

        assert_eq!(
            ops,
            vec![Operation::Grow {
                partition: guid(1),
                len: mib(90)
            }]
        );
    }

    #[test]
    fn recreates_swap_instead_of_copying_it() {
        let current = layout(
            100,
            vec![
                partition(1, Content::Ext4, 1, 40),
                partition(2, Content::Swap, 41, 10),
            ],
        );
        let target = target_of(&current, &[(1, 1, 60), (2, 61, 10)]);

        let plan = plan(&current, &target).unwrap();

        assert_eq!(plan.sectors_to_copy(), Sectors(0));
        assert_eq!(
            plan.operations[0],
            Operation::RemoveSwap { partition: guid(2) }
        );
        assert!(matches!(
            plan.operations.last(),
            Some(Operation::RecreateSwap { partition }) if partition.extent.start == at(61)
        ));
    }

    #[test]
    fn moves_windows_first_when_both_moves_are_possible() {
        let current = layout(
            200,
            vec![
                partition(1, Content::Ext4, 10, 20),
                partition(2, Content::Ntfs, 50, 20),
            ],
        );
        let target = target_of(&current, &[(1, 1, 20), (2, 30, 20)]);

        let ops = operations(&current, &target);

        assert!(matches!(ops[0], Operation::Move { partition, .. } if partition == guid(2)));
    }

    #[test]
    fn swaps_two_partitions_by_parking_one_in_free_space() {
        let current = layout(
            200,
            vec![
                partition(1, Content::Ext4, 1, 20),
                partition(2, Content::Ext4, 21, 20),
            ],
        );
        let target = target_of(&current, &[(1, 21, 20), (2, 1, 20)]);

        let ops = operations(&current, &target);

        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn refuses_to_reorder_without_room_to_park() {
        let current = layout(
            42,
            vec![
                partition(1, Content::Ext4, 1, 20),
                partition(2, Content::Ext4, 21, 20),
            ],
        );
        let target = target_of(&current, &[(1, 21, 20), (2, 1, 20)]);

        let error = plan(&current, &target).unwrap_err();

        assert!(matches!(error, PlanError::Refused(r) if r.reason.contains("free space")));
    }

    #[test]
    fn passes_validation_refusals_through() {
        let current = layout(100, vec![partition(1, Content::EfiSystem, 1, 10)]);
        let target = target_of(&current, &[(1, 20, 10)]);

        assert!(matches!(
            plan(&current, &target),
            Err(PlanError::Refused(_))
        ));
    }
}
