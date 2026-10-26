use std::collections::BTreeSet;

use crate::layout::{Content, Extent, Geometry, Layout, Partition, Target};
use crate::refusal::Refusal;
use crate::units::Sectors;

/// Checks that BootShift can safely turn `current` into `target`.
pub fn validate(current: &Layout, target: &Target) -> Result<(), Refusal> {
    check_same_partitions(current, target)?;
    for partition in &current.partitions {
        if let Some(&extent) = target.get(&partition.unique_guid) {
            check_partition(&current.geometry, partition, extent)?;
        }
    }
    check_no_overlap(current, target)
}

fn check_same_partitions(current: &Layout, target: &Target) -> Result<(), Refusal> {
    let guids: BTreeSet<_> = current.partitions.iter().map(|p| p.unique_guid).collect();
    let has_duplicates = guids.len() != current.partitions.len();
    if has_duplicates || !guids.iter().eq(target.keys()) {
        return refuse(
            "The new layout doesn't list exactly the partitions on this disk.".to_string(),
            "Reload the disk and arrange the partitions again.",
        );
    }
    Ok(())
}

fn check_partition(geometry: &Geometry, partition: &Partition, new: Extent) -> Result<(), Refusal> {
    let label = partition.label();
    let content = partition.content;
    let moves = new.start != partition.extent.start;
    let shrinks = new.len < partition.extent.len;
    let grows = new.len > partition.extent.len;

    if !(moves || shrinks || grows) {
        return Ok(());
    }
    if new.len == Sectors(0) {
        return refuse(format!("{label} would have no space."), "Give it a size.");
    }
    if !geometry.usable.contains(new) {
        return refuse(
            format!("{label} would reach outside the usable area of the disk."),
            "Keep it within the disk.",
        );
    }
    match content {
        Content::EfiSystem => {
            return refuse(
                format!(
                    "{label} is the EFI system partition, which holds BootShift's recovery system during the change."
                ),
                "Leave the EFI system partition where it is, at its current size.",
            );
        }
        Content::BitLocker => {
            return refuse(
                format!("{label} is encrypted with BitLocker."),
                "In Windows, back up your recovery key, turn off device encryption, then try again.",
            );
        }
        _ => {}
    }
    if moves && !new.start.is_aligned(geometry.alignment) {
        return refuse(
            format!("{label} would start at a position that isn't on a 1 MiB boundary."),
            "Place it on a 1 MiB boundary.",
        );
    }
    if (shrinks || grows) && !new.end().is_aligned(geometry.alignment) {
        return refuse(
            format!("{label} would end at a position that isn't on a 1 MiB boundary."),
            "Make its size a whole number of MiB.",
        );
    }
    if grows && !content.can_grow() {
        return refuse(
            format!(
                "BootShift can move {label} but can't grow the {} data inside it.",
                content.name()
            ),
            "Keep its current size.",
        );
    }
    if shrinks {
        check_shrink(geometry, partition, new.len)?;
    }
    Ok(())
}

fn check_shrink(
    geometry: &Geometry,
    partition: &Partition,
    new_len: Sectors,
) -> Result<(), Refusal> {
    let label = partition.label();
    match partition.content {
        Content::Swap => Ok(()),
        Content::Xfs => refuse(
            format!("{label} uses XFS, which can't be shrunk."),
            "Keep its current size or make it larger.",
        ),
        content if !content.can_shrink() => refuse(
            format!(
                "BootShift can move {label} but can't shrink the {} data inside it.",
                content.name()
            ),
            "Keep its current size or make it larger.",
        ),
        _ => match partition.min_len {
            None => refuse(
                format!("BootShift couldn't measure how small {label} can be made."),
                "Check the filesystem for errors, then try again.",
            ),
            Some(min_len) if new_len < min_len => {
                let min_mib = whole_mib(geometry, min_len);
                refuse(
                    format!(
                        "{label} can't be smaller than {min_mib} MiB, because that space holds files."
                    ),
                    &format!("Make it at least {min_mib} MiB, or delete some files first."),
                )
            }
            Some(_) => Ok(()),
        },
    }
}

fn check_no_overlap(current: &Layout, target: &Target) -> Result<(), Refusal> {
    let mut placed: Vec<(Extent, &Partition)> = current
        .partitions
        .iter()
        .filter_map(|p| target.get(&p.unique_guid).map(|&extent| (extent, p)))
        .collect();
    placed.sort_by_key(|(extent, _)| extent.start);

    for pair in placed.windows(2) {
        let [(first, a), (second, b)] = pair else {
            continue;
        };
        if first.overlaps(*second) {
            return refuse(
                format!("{} and {} would overlap.", a.label(), b.label()),
                "Move one of them so they don't share space.",
            );
        }
    }
    Ok(())
}

fn whole_mib(geometry: &Geometry, len: Sectors) -> u64 {
    len.to_bytes(geometry.sector_size).div_ceil(1 << 20)
}

fn refuse<T>(reason: String, fix: &str) -> Result<T, Refusal> {
    Err(Refusal::new(reason, fix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn refusal(current: &Layout, target: &Target) -> String {
        validate(current, target).unwrap_err().reason
    }

    #[test]
    fn accepts_unchanged_layout() {
        let current = layout(100, vec![partition(1, Content::Ext4, 1, 50)]);
        assert!(validate(&current, &unchanged(&current)).is_ok());
    }

    #[test]
    fn accepts_moving_and_growing_into_free_space() {
        let current = layout(100, vec![partition(1, Content::Ext4, 1, 50)]);
        let target = target_of(&current, &[(1, 10, 60)]);
        assert!(validate(&current, &target).is_ok());
    }

    #[test]
    fn refuses_a_target_missing_a_partition() {
        let current = layout(
            100,
            vec![
                partition(1, Content::Ext4, 1, 20),
                partition(2, Content::Ext4, 30, 20),
            ],
        );
        let mut target = unchanged(&current);
        target.remove(&guid(2));
        assert!(refusal(&current, &target).contains("exactly the partitions"));
    }

    #[test]
    fn refuses_moving_the_efi_system_partition() {
        let current = layout(100, vec![partition(1, Content::EfiSystem, 1, 10)]);
        let target = target_of(&current, &[(1, 20, 10)]);
        assert!(refusal(&current, &target).contains("EFI system partition"));
    }

    #[test]
    fn refuses_changing_a_bitlocker_volume() {
        let current = layout(100, vec![partition(1, Content::BitLocker, 1, 10)]);
        let target = target_of(&current, &[(1, 20, 10)]);
        assert!(refusal(&current, &target).contains("BitLocker"));
    }

    #[test]
    fn refuses_reaching_outside_the_usable_area() {
        let current = layout(100, vec![partition(1, Content::Ext4, 1, 10)]);
        let target = target_of(&current, &[(1, 95, 10)]);
        assert!(refusal(&current, &target).contains("outside the usable area"));
    }

    #[test]
    fn refuses_unaligned_start_after_a_move() {
        let current = layout(100, vec![partition(1, Content::Ext4, 1, 10)]);
        let mut target = unchanged(&current);
        target.insert(guid(1), Extent::new(at(20) + Sectors(1), mib(10)));
        assert!(refusal(&current, &target).contains("start"));
    }

    #[test]
    fn refuses_overlapping_partitions() {
        let current = layout(
            100,
            vec![
                partition(1, Content::Ext4, 1, 20),
                partition(2, Content::Ext4, 30, 20),
            ],
        );
        let target = target_of(&current, &[(1, 1, 40), (2, 30, 20)]);
        assert!(refusal(&current, &target).contains("overlap"));
    }

    #[test]
    fn refuses_shrinking_xfs() {
        let current = layout(100, vec![partition(1, Content::Xfs, 1, 20)]);
        let target = target_of(&current, &[(1, 1, 10)]);
        assert!(refusal(&current, &target).contains("XFS"));
    }

    #[test]
    fn refuses_growing_contents_it_cannot_read() {
        let current = layout(100, vec![partition(1, Content::Opaque, 1, 20)]);
        let target = target_of(&current, &[(1, 1, 30)]);
        assert!(refusal(&current, &target).contains("can't grow"));
    }

    #[test]
    fn refuses_shrinking_below_the_minimum_size() {
        let mut ext4 = partition(1, Content::Ext4, 1, 40);
        ext4.min_len = Some(mib(30));
        let current = layout(100, vec![ext4]);
        let target = target_of(&current, &[(1, 1, 20)]);
        assert!(refusal(&current, &target).contains("can't be smaller than 30 MiB"));
    }

    #[test]
    fn refuses_shrinking_when_the_minimum_size_is_unknown() {
        let mut ext4 = partition(1, Content::Ext4, 1, 40);
        ext4.min_len = None;
        let current = layout(100, vec![ext4]);
        let target = target_of(&current, &[(1, 1, 20)]);
        assert!(refusal(&current, &target).contains("couldn't measure"));
    }

    #[test]
    fn allows_shrinking_swap_regardless_of_contents() {
        let mut swap = partition(1, Content::Swap, 1, 40);
        swap.min_len = None;
        let current = layout(100, vec![swap]);
        let target = target_of(&current, &[(1, 1, 10)]);
        assert!(validate(&current, &target).is_ok());
    }
}
