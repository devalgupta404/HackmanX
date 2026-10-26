//! How dragging changes the target layout. Every request is snapped to the disk's alignment and
//! clamped so partitions never overlap, never leave the disk, and only shrink or grow when their
//! contents allow it. These are `core::validate`'s rules, applied while the user drags; the
//! helper checks the result again before it plans anything.
//!
//! Requests are in sectors as `f64`, because a pointer drag lands between sectors. Each edit
//! returns the partition's new extent, or `None` when the request can't change anything.

use bootshift_core::layout::{Content, Extent, Guid, Layout, Partition, Target};
use bootshift_core::units::{Lba, Sectors};

/// The bytes one arrow-key press moves or resizes a partition by.
pub const KEY_STEP_BYTES: u64 = 1 << 30;

/// The smallest the partition may become.
pub fn min_len(layout: &Layout, partition: &Partition) -> Sectors {
    let alignment = layout.geometry.alignment.0;
    let len = if !partition.content.can_shrink() {
        partition.extent.len.0
    } else if partition.content == Content::Swap {
        alignment
    } else {
        align_up(
            partition.min_len.unwrap_or(partition.extent.len).0,
            alignment,
        )
    };
    Sectors(len)
}

/// The largest the partition may become before neighbours are considered. `None` is unbounded.
pub fn max_len(partition: &Partition) -> Option<Sectors> {
    (!partition.content.can_grow()).then_some(partition.extent.len)
}

/// Slides a partition to start near `requested_start`, keeping its length.
pub fn move_to(
    layout: &Layout,
    target: &Target,
    guid: Guid,
    requested_start: f64,
) -> Option<Extent> {
    let (partition, current) = editable(layout, target, guid)?;
    let alignment = layout.geometry.alignment.0;
    // A resized partition must end on a boundary. Its start is about to land on one, so its
    // length has to be a whole number of boundaries too, or there is nowhere it may go.
    if current.len != partition.extent.len && current.len.0 % alignment != 0 {
        return None;
    }
    let (left, right) = neighbour_bounds(layout, target, guid);
    let lowest = align_up(left, alignment);
    let highest = align_down(right.checked_sub(current.len.0)?, alignment);
    let start = clamp(snap(requested_start, alignment), lowest, highest)?;
    changed(current, Extent::new(Lba(start), current.len))
}

/// Moves the partition's end to near `requested_end`, keeping its start.
pub fn set_end(layout: &Layout, target: &Target, guid: Guid, requested_end: f64) -> Option<Extent> {
    let (partition, current) = editable(layout, target, guid)?;
    let alignment = layout.geometry.alignment.0;
    let (_, right) = neighbour_bounds(layout, target, guid);
    let start = current.start.0;
    let shortest = align_up(start + min_len(layout, partition).0, alignment);
    let longest = match max_len(partition) {
        Some(max) => align_down(right.min(start + max.0), alignment),
        None => align_down(right, alignment),
    };
    let end = clamp(snap(requested_end, alignment), shortest, longest)?;
    changed(
        current,
        Extent::new(current.start, Sectors(end.checked_sub(start)?)),
    )
}

/// Moves the partition's start to near `requested_start`, keeping its end.
pub fn set_start(
    layout: &Layout,
    target: &Target,
    guid: Guid,
    requested_start: f64,
) -> Option<Extent> {
    let (partition, current) = editable(layout, target, guid)?;
    let alignment = layout.geometry.alignment.0;
    let end = current.end().0;
    // Resizing needs the end on a boundary too, and this edit can't move the end.
    if end % alignment != 0 {
        return None;
    }
    let (left, _) = neighbour_bounds(layout, target, guid);
    let mut earliest = align_up(left, alignment);
    if let Some(max) = max_len(partition) {
        earliest = earliest.max(align_up(end.saturating_sub(max.0), alignment));
    }
    let latest = align_down(end.checked_sub(min_len(layout, partition).0)?, alignment);
    let start = clamp(snap(requested_start, alignment), earliest, latest)?;
    changed(current, Extent::new(Lba(start), Sectors(end - start)))
}

pub fn is_changed(layout: &Layout, target: &Target, guid: Guid) -> bool {
    match (layout.partition(guid), target.get(&guid)) {
        (Some(partition), Some(after)) => partition.extent != *after,
        _ => false,
    }
}

/// The free space a partition may use: from the end of the one before it to the start of the
/// one after it, in the target layout, bounded by the usable region.
pub fn neighbour_bounds(layout: &Layout, target: &Target, guid: Guid) -> (u64, u64) {
    let usable = layout.geometry.usable;
    let mut left = usable.start.0;
    let mut right = usable.end().0;
    let Some(me) = target.get(&guid) else {
        return (left, right);
    };
    for (other, extent) in target {
        if *other == guid {
            continue;
        }
        if extent.end() <= me.start {
            left = left.max(extent.end().0);
        }
        if extent.start >= me.end() {
            right = right.min(extent.start.0);
        }
    }
    (left, right)
}

/// Grows `guid` toward `goal_len` sectors: into free space first, then, if that isn't enough, by
/// shrinking and shifting a resizable neighbour, on the right then the left. Returns the whole
/// changed target. Every step goes through the same clamps as a drag, so the result is one
/// `validate` accepts, and the helper checks it again before planning.
pub fn grow_with_neighbours(layout: &Layout, base: &Target, guid: Guid, goal_len: u64) -> Target {
    let mut target = base.clone();
    if !layout.partition(guid).is_some_and(|p| p.content.can_grow()) {
        return target;
    }
    grow_rightward(layout, &mut target, guid, goal_len);
    grow_leftward(layout, &mut target, guid, goal_len);
    target
}

/// How far `guid` can grow, in sectors added, taking free space and shrinking resizable
/// neighbours as far as they will go.
pub fn grow_capacity(layout: &Layout, base: &Target, guid: Guid) -> Sectors {
    let before = len_of(base, guid);
    let grown = grow_with_neighbours(layout, base, guid, u64::MAX);
    Sectors(len_of(&grown, guid).saturating_sub(before))
}

fn grow_rightward(layout: &Layout, target: &mut Target, guid: Guid, goal_len: u64) {
    extend_end(layout, target, guid, goal_len);
    if len_of(target, guid) >= goal_len {
        return;
    }
    let Some(neighbour) = right_neighbour(target, guid) else {
        return;
    };
    if !layout
        .partition(neighbour)
        .is_some_and(|p| p.content.can_shrink() && p.content.can_move())
    {
        return;
    }
    let Some(before) = target.get(&neighbour).copied() else {
        return;
    };
    let deficit = goal_len.saturating_sub(len_of(target, guid));
    // Shrink the neighbour from its end, then slide it right so it ends where it did. That frees
    // the gap on our side to grow into.
    if let Some(shrunk) = set_end(
        layout,
        target,
        neighbour,
        before.end().0.saturating_sub(deficit) as f64,
    ) {
        target.insert(neighbour, shrunk);
    }
    let new_len = len_of(target, neighbour);
    if let Some(moved) = move_to(
        layout,
        target,
        neighbour,
        before.end().0.saturating_sub(new_len) as f64,
    ) {
        target.insert(neighbour, moved);
    }
    extend_end(layout, target, guid, goal_len);
}

fn grow_leftward(layout: &Layout, target: &mut Target, guid: Guid, goal_len: u64) {
    if len_of(target, guid) >= goal_len {
        return;
    }
    extend_start(layout, target, guid, goal_len);
    if len_of(target, guid) >= goal_len {
        return;
    }
    let Some(neighbour) = left_neighbour(target, guid) else {
        return;
    };
    if !layout
        .partition(neighbour)
        .is_some_and(|p| p.content.can_shrink())
    {
        return;
    }
    let Some(before) = target.get(&neighbour).copied() else {
        return;
    };
    let deficit = goal_len.saturating_sub(len_of(target, guid));
    // Shrink the left neighbour from its end; the freed space lands right next to us, so we only
    // have to pull our start over it. No move is needed.
    if let Some(shrunk) = set_end(
        layout,
        target,
        neighbour,
        before.end().0.saturating_sub(deficit) as f64,
    ) {
        target.insert(neighbour, shrunk);
    }
    extend_start(layout, target, guid, goal_len);
}

/// Pushes `guid`'s end out to reach `goal_len`, as far as the clamps allow.
fn extend_end(layout: &Layout, target: &mut Target, guid: Guid, goal_len: u64) {
    let Some(current) = target.get(&guid).copied() else {
        return;
    };
    let want = current.start.0.saturating_add(goal_len);
    if let Some(extent) = set_end(layout, target, guid, want as f64) {
        target.insert(guid, extent);
    }
}

/// Pulls `guid`'s start back to reach `goal_len`, as far as the clamps allow.
fn extend_start(layout: &Layout, target: &mut Target, guid: Guid, goal_len: u64) {
    let Some(current) = target.get(&guid).copied() else {
        return;
    };
    let want = current.end().0.saturating_sub(goal_len);
    if let Some(extent) = set_start(layout, target, guid, want as f64) {
        target.insert(guid, extent);
    }
}

/// The partition nearest to the right of `guid` in the target, if any.
fn right_neighbour(target: &Target, guid: Guid) -> Option<Guid> {
    let me = *target.get(&guid)?;
    target
        .iter()
        .filter(|(other, extent)| **other != guid && extent.start >= me.end())
        .min_by_key(|(_, extent)| extent.start.0)
        .map(|(other, _)| *other)
}

/// The partition nearest to the left of `guid` in the target, if any.
fn left_neighbour(target: &Target, guid: Guid) -> Option<Guid> {
    let me = *target.get(&guid)?;
    target
        .iter()
        .filter(|(other, extent)| **other != guid && extent.end() <= me.start)
        .max_by_key(|(_, extent)| extent.end().0)
        .map(|(other, _)| *other)
}

fn len_of(target: &Target, guid: Guid) -> u64 {
    target.get(&guid).map_or(0, |extent| extent.len.0)
}

fn editable<'a>(
    layout: &'a Layout,
    target: &Target,
    guid: Guid,
) -> Option<(&'a Partition, Extent)> {
    let partition = layout.partition(guid)?;
    let current = *target.get(&guid)?;
    partition.content.can_move().then_some((partition, current))
}

fn changed(current: Extent, next: Extent) -> Option<Extent> {
    (next != current).then_some(next)
}

/// Rounds a requested sector to the nearest boundary. A request left of sector 0 snaps to 0.
fn snap(value: f64, alignment: u64) -> u64 {
    let units = (value / alignment as f64).round();
    if units <= 0.0 {
        0
    } else {
        (units as u64).saturating_mul(alignment)
    }
}

fn align_up(value: u64, alignment: u64) -> u64 {
    Lba(value).align_up(Sectors(alignment)).0
}

fn align_down(value: u64, alignment: u64) -> u64 {
    value - value.checked_rem(alignment).unwrap_or(0)
}

/// `None` when the bounds leave no room, so an impossible edit changes nothing at all.
fn clamp(value: u64, lowest: u64, highest: u64) -> Option<u64> {
    (lowest <= highest).then(|| value.clamp(lowest, highest))
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::Geometry;
    use bootshift_core::validate::validate;
    use proptest::prelude::*;

    use super::*;

    const MIB: u64 = 2048;

    fn guid(number: u32) -> Guid {
        Guid([number as u8; 16])
    }

    fn partition(number: u32, content: Content, start_mib: u64, len_mib: u64) -> Partition {
        Partition {
            number,
            unique_guid: guid(number),
            type_guid: Guid([0; 16]),
            name: String::new(),
            attributes: 0,
            extent: Extent::new(Lba(start_mib * MIB), Sectors(len_mib * MIB)),
            content,
            used: Sectors(len_mib * MIB / 4),
            min_len: Some(Sectors(len_mib * MIB / 4)),
        }
    }

    /// A 1 GiB disk, usable from 1 MiB to 1023 MiB.
    fn layout(partitions: Vec<Partition>) -> Layout {
        Layout {
            geometry: Geometry {
                sector_size: 512,
                usable: Extent::new(Lba(MIB), Sectors(1022 * MIB)),
                alignment: Sectors(MIB),
            },
            partitions,
        }
    }

    fn target_of(layout: &Layout) -> Target {
        layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect()
    }

    fn mib_extent(start_mib: u64, len_mib: u64) -> Extent {
        Extent::new(Lba(start_mib * MIB), Sectors(len_mib * MIB))
    }

    fn at_mib(mib: f64) -> f64 {
        mib * MIB as f64
    }

    #[test]
    fn a_move_snaps_to_the_nearest_mib() {
        let layout = layout(vec![partition(1, Content::Ext4, 100, 100)]);
        let moved = move_to(&layout, &target_of(&layout), guid(1), at_mib(200.4));
        assert_eq!(moved, Some(mib_extent(200, 100)));
    }

    #[test]
    fn a_move_stops_at_the_neighbour() {
        let layout = layout(vec![
            partition(1, Content::Ext4, 100, 100),
            partition(2, Content::Ext4, 300, 100),
        ]);
        let moved = move_to(&layout, &target_of(&layout), guid(1), at_mib(280.0));
        assert_eq!(moved, Some(mib_extent(200, 100)));
    }

    #[test]
    fn a_move_stays_on_the_disk() {
        let layout = layout(vec![partition(1, Content::Ext4, 100, 100)]);
        let target = target_of(&layout);
        assert_eq!(
            move_to(&layout, &target, guid(1), at_mib(-50.0)),
            Some(mib_extent(1, 100))
        );
        assert_eq!(
            move_to(&layout, &target, guid(1), at_mib(5000.0)),
            Some(mib_extent(923, 100))
        );
    }

    #[test]
    fn the_efi_system_partition_and_bitlocker_never_move_or_resize() {
        let layout = layout(vec![
            partition(1, Content::EfiSystem, 1, 100),
            partition(2, Content::BitLocker, 200, 100),
        ]);
        let target = target_of(&layout);
        for number in [1, 2] {
            assert_eq!(move_to(&layout, &target, guid(number), at_mib(500.0)), None);
            assert_eq!(set_end(&layout, &target, guid(number), at_mib(900.0)), None);
            assert_eq!(
                set_start(&layout, &target, guid(number), at_mib(150.0)),
                None
            );
        }
    }

    #[test]
    fn a_shrink_stops_at_the_filesystems_minimum() {
        let layout = layout(vec![partition(1, Content::Ext4, 100, 100)]);
        let shrunk = set_end(&layout, &target_of(&layout), guid(1), at_mib(110.0));
        assert_eq!(shrunk, Some(mib_extent(100, 25)));
    }

    #[test]
    fn a_minimum_that_is_not_a_whole_mib_rounds_up() {
        let mut ext4 = partition(1, Content::Ext4, 100, 100);
        ext4.min_len = Some(Sectors(10 * MIB + 1));
        let layout = layout(vec![ext4]);
        let shrunk = set_end(&layout, &target_of(&layout), guid(1), at_mib(100.0));
        assert_eq!(shrunk, Some(mib_extent(100, 11)));
    }

    #[test]
    fn swap_shrinks_down_to_one_alignment_unit() {
        let layout = layout(vec![partition(1, Content::Swap, 100, 100)]);
        let shrunk = set_end(&layout, &target_of(&layout), guid(1), at_mib(0.0));
        assert_eq!(shrunk, Some(mib_extent(100, 1)));
    }

    #[test]
    fn xfs_grows_but_never_shrinks() {
        let layout = layout(vec![partition(1, Content::Xfs, 100, 100)]);
        let target = target_of(&layout);
        assert_eq!(set_end(&layout, &target, guid(1), at_mib(150.0)), None);
        assert_eq!(
            set_end(&layout, &target, guid(1), at_mib(300.0)),
            Some(mib_extent(100, 200))
        );
    }

    #[test]
    fn unrecognised_contents_keep_their_size() {
        let layout = layout(vec![partition(1, Content::Opaque, 100, 100)]);
        let target = target_of(&layout);
        assert_eq!(set_end(&layout, &target, guid(1), at_mib(300.0)), None);
        assert_eq!(set_start(&layout, &target, guid(1), at_mib(50.0)), None);
        assert_eq!(
            move_to(&layout, &target, guid(1), at_mib(300.0)),
            Some(mib_extent(300, 100))
        );
    }

    #[test]
    fn dragging_the_left_edge_keeps_the_end() {
        let layout = layout(vec![
            partition(1, Content::Ext4, 1, 50),
            partition(2, Content::Ext4, 100, 100),
        ]);
        let target = target_of(&layout);
        assert_eq!(
            set_start(&layout, &target, guid(2), at_mib(20.0)),
            Some(mib_extent(51, 149))
        );
        assert_eq!(
            set_start(&layout, &target, guid(2), at_mib(190.0)),
            Some(mib_extent(175, 25))
        );
    }

    #[test]
    fn growing_stops_at_the_next_partition() {
        let layout = layout(vec![
            partition(1, Content::Ntfs, 100, 100),
            partition(2, Content::Ext4, 300, 100),
        ]);
        let grown = set_end(&layout, &target_of(&layout), guid(1), at_mib(900.0));
        assert_eq!(grown, Some(mib_extent(100, 200)));
    }

    #[test]
    fn a_partition_boxed_in_by_its_neighbours_does_not_jump() {
        let layout = layout(vec![
            partition(1, Content::Ext4, 1, 100),
            partition(2, Content::Ext4, 101, 100),
            partition(3, Content::Ext4, 201, 100),
        ]);
        let target = target_of(&layout);
        assert_eq!(move_to(&layout, &target, guid(2), at_mib(500.0)), None);
        assert_eq!(move_to(&layout, &target, guid(2), at_mib(0.0)), None);
    }

    #[test]
    fn a_partition_without_a_measured_minimum_cannot_shrink() {
        let mut ntfs = partition(1, Content::Ntfs, 100, 100);
        ntfs.min_len = None;
        let layout = layout(vec![ntfs]);
        let shrunk = set_end(&layout, &target_of(&layout), guid(1), at_mib(120.0));
        assert_eq!(shrunk, None);
    }

    #[test]
    fn an_end_off_a_boundary_cannot_be_kept_while_resizing_from_the_left() {
        let mut ext4 = partition(1, Content::Ext4, 100, 100);
        ext4.extent.len = Sectors(100 * MIB + 7);
        let layout = layout(vec![ext4]);
        let resized = set_start(&layout, &target_of(&layout), guid(1), at_mib(150.0));
        assert_eq!(resized, None);
    }

    #[test]
    fn a_resized_partition_whose_end_would_leave_a_boundary_does_not_move() {
        let mut ext4 = partition(1, Content::Ext4, 100, 100);
        ext4.extent.start = Lba(100 * MIB + 1);
        let layout = layout(vec![ext4]);
        let mut target = target_of(&layout);
        let grown = set_end(&layout, &target, guid(1), at_mib(300.0)).unwrap();
        target.insert(guid(1), grown);
        assert_eq!(move_to(&layout, &target, guid(1), at_mib(400.0)), None);
        assert_eq!(validate(&layout, &target), Ok(()));
    }

    #[test]
    fn neighbour_bounds_follow_the_target_not_the_disk() {
        let layout = layout(vec![
            partition(1, Content::Ext4, 1, 100),
            partition(2, Content::Ext4, 300, 100),
        ]);
        let mut target = target_of(&layout);
        target.insert(guid(1), mib_extent(150, 100));
        assert_eq!(
            neighbour_bounds(&layout, &target, guid(2)),
            (250 * MIB, 1023 * MIB)
        );
    }

    #[test]
    fn growing_pulls_space_from_a_shrinkable_right_neighbour() {
        // A full Btrfs partition butted up against a roomy ext4 one, no gap between them.
        let layout = layout(vec![
            partition(1, Content::Btrfs, 100, 100),
            partition(2, Content::Ext4, 200, 200),
        ]);
        let base = target_of(&layout);
        // Asking for 250 MiB needs 150 MiB more than it has; that can only come from neighbour 2.
        let grown = grow_with_neighbours(&layout, &base, guid(1), 250 * MIB);
        assert!(len_of(&grown, guid(1)) >= 250 * MIB);
        assert!(len_of(&grown, guid(2)) < 200 * MIB);
        assert_eq!(validate(&layout, &grown), Ok(()));
    }

    #[test]
    fn growing_takes_free_space_before_touching_a_neighbour() {
        // 50 MiB gap between the two; growing by 40 MiB should leave the neighbour untouched.
        let layout = layout(vec![
            partition(1, Content::Btrfs, 100, 100),
            partition(2, Content::Ext4, 250, 100),
        ]);
        let base = target_of(&layout);
        let grown = grow_with_neighbours(&layout, &base, guid(1), 140 * MIB);
        assert_eq!(len_of(&grown, guid(1)), 140 * MIB);
        assert_eq!(len_of(&grown, guid(2)), 100 * MIB);
        assert_eq!(validate(&layout, &grown), Ok(()));
    }

    #[test]
    fn grow_capacity_never_exceeds_what_actually_fits() {
        let layout = layout(vec![
            partition(1, Content::Btrfs, 100, 100),
            partition(2, Content::Ext4, 200, 200),
        ]);
        let base = target_of(&layout);
        let capacity = grow_capacity(&layout, &base, guid(1)).0;
        let grown = grow_with_neighbours(&layout, &base, guid(1), 100 * MIB + capacity);
        assert_eq!(len_of(&grown, guid(1)), 100 * MIB + capacity);
        assert_eq!(validate(&layout, &grown), Ok(()));
    }

    #[derive(Clone, Copy, Debug)]
    enum Edit {
        Move(f64),
        Start(f64),
        End(f64),
    }

    fn content() -> impl Strategy<Value = Content> {
        prop_oneof![
            Just(Content::EfiSystem),
            Just(Content::Ext4),
            Just(Content::Ntfs),
            Just(Content::Btrfs),
            Just(Content::Xfs),
            Just(Content::Swap),
            Just(Content::BitLocker),
            Just(Content::Opaque),
        ]
    }

    /// Up to five partitions laid out left to right with gaps, some with odd sector counts.
    fn any_layout() -> impl Strategy<Value = Layout> {
        prop::collection::vec(
            (content(), 0u64..60, 1u64..150, 0u64..4, any::<bool>()),
            1..6,
        )
        .prop_map(|specs| {
            let mut start = MIB;
            let mut partitions = Vec::new();
            for (index, (content, gap, len_mib, odd, has_min)) in specs.into_iter().enumerate() {
                start += gap * MIB;
                let len = len_mib * MIB + odd;
                if start + len > 1023 * MIB {
                    break;
                }
                let number = index as u32 + 1;
                let mut p = partition(number, content, 0, 0);
                p.extent = Extent::new(Lba(start), Sectors(len));
                p.used = Sectors(len / 3);
                p.min_len = has_min.then_some(Sectors(len / 3));
                partitions.push(p);
                start += len;
            }
            layout(partitions)
        })
        .prop_filter("at least one partition", |l| !l.partitions.is_empty())
    }

    fn any_edits() -> impl Strategy<Value = Vec<(usize, Edit)>> {
        let request = -100.0f64..(1100.0 * MIB as f64);
        let edit = prop_oneof![
            request.clone().prop_map(Edit::Move),
            request.clone().prop_map(Edit::Start),
            request.prop_map(Edit::End),
        ];
        prop::collection::vec((0usize..5, edit), 1..20)
    }

    proptest! {
        /// Whatever the user drags, the layout stays one `core::validate` accepts.
        #[test]
        fn every_edit_keeps_the_target_valid(layout in any_layout(), edits in any_edits()) {
            let mut target = target_of(&layout);
            for (index, edit) in edits {
                let guid = layout.partitions[index % layout.partitions.len()].unique_guid;
                let next = match edit {
                    Edit::Move(at) => move_to(&layout, &target, guid, at),
                    Edit::Start(at) => set_start(&layout, &target, guid, at),
                    Edit::End(at) => set_end(&layout, &target, guid, at),
                };
                if let Some(extent) = next {
                    target.insert(guid, extent);
                }
                prop_assert_eq!(validate(&layout, &target), Ok(()));
            }
        }
    }
}
