//! Rewriting a real GPT built with gptman, including a power cut at every write.

#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Seek, SeekFrom};

use bootshift_core::layout::{Content, Extent, Guid, Partition};
use bootshift_core::units::{Lba, Sectors};
use bootshift_disk::{CrashDisk, GptError, GptImage, MemDisk, PendingWrites};
use gptman::{GPT, GPTHeader, GPTPartitionEntry};

const SECTOR: u64 = 512;
const MIB: u64 = 2048;
const DISK_SECTORS: u64 = 16 * MIB;
const LINUX_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];
const LINUX_SWAP: [u8; 16] = [
    0x6D, 0xFD, 0x57, 0x06, 0xAB, 0xA4, 0xC4, 0x43, 0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F, 0x4F,
];

fn entry(
    id: u8,
    type_guid: [u8; 16],
    start_mib: u64,
    len_mib: u64,
    name: &str,
) -> GPTPartitionEntry {
    GPTPartitionEntry {
        partition_type_guid: type_guid,
        unique_partition_guid: [id; 16],
        starting_lba: start_mib * MIB,
        ending_lba: (start_mib + len_mib) * MIB - 1,
        attribute_bits: u64::from(id) << 48,
        partition_name: name.into(),
    }
}

/// A 16 MiB disk with root at 1 MiB, swap at 5 MiB and home at 9 MiB.
fn disk() -> MemDisk {
    let mut cursor = Cursor::new(vec![0; (DISK_SECTORS * SECTOR) as usize]);
    let mut gpt = GPT::new_from(&mut cursor, SECTOR, [0xD1; 16]).unwrap();
    gpt[1] = entry(1, LINUX_DATA, 1, 2, "root");
    gpt[2] = entry(2, LINUX_SWAP, 5, 1, "swap");
    gpt[3] = entry(3, LINUX_DATA, 9, 2, "home");
    gpt.write_into(&mut cursor).unwrap();
    GPT::write_protective_mbr_into(&mut cursor, SECTOR).unwrap();
    MemDisk::from_bytes(cursor.into_inner(), SECTOR as u32).unwrap()
}

fn partitions(image: &GptImage) -> Vec<Partition> {
    image
        .entries()
        .unwrap()
        .into_iter()
        .map(|entry| Partition {
            number: entry.number,
            unique_guid: entry.unique_guid,
            type_guid: entry.type_guid,
            name: entry.name,
            attributes: entry.attributes,
            extent: entry.extent,
            content: Content::Opaque,
            used: entry.extent.len,
            min_len: None,
        })
        .collect()
}

fn moved(mut partitions: Vec<Partition>, number: u32, start_mib: u64) -> Vec<Partition> {
    for partition in &mut partitions {
        if partition.number == number {
            partition.extent.start = Lba(start_mib * MIB);
        }
    }
    partitions
}

/// First and last LBA of each used entry.
type Extents = Vec<(u64, u64)>;

/// The used extents of one copy of the table, read on its own; `None` if that copy is damaged.
fn copy_extents(disk: &MemDisk, header_lba: u64) -> Option<Extents> {
    let mut cursor = Cursor::new(disk.bytes());
    cursor.seek(SeekFrom::Start(header_lba * SECTOR)).ok()?;
    let header = GPTHeader::read_from(&mut cursor).ok()?;
    let entries = header.read_partitions(&mut cursor, SECTOR).ok()?;
    Some(
        entries
            .iter()
            .filter(|e| e.is_used())
            .map(|e| (e.starting_lba, e.ending_lba))
            .collect(),
    )
}

fn both_copies(disk: &MemDisk) -> (Option<Extents>, Option<Extents>) {
    (copy_extents(disk, 1), copy_extents(disk, DISK_SECTORS - 1))
}

#[test]
fn reads_entries_and_the_usable_area() {
    let image = GptImage::read(&disk()).unwrap();
    let entries = image.entries().unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[2].name, "home");
    assert_eq!(
        entries[2].extent,
        Extent::new(Lba(9 * MIB), Sectors(2 * MIB))
    );
    assert_eq!(
        image.usable(),
        Extent::new(Lba(34), Sectors(DISK_SECTORS - 34 - 33))
    );
}

#[test]
fn moving_a_partition_rewrites_both_copies_and_keeps_everything_else() {
    let mut disk = disk();
    let original = GptImage::read(&disk).unwrap();
    let mbr_before = disk.bytes()[..SECTOR as usize].to_vec();

    let target = original
        .with_partitions(&moved(partitions(&original), 3, 12), &[])
        .unwrap();
    target.write(&mut disk).unwrap();

    let expected = vec![
        (MIB, 3 * MIB - 1),
        (5 * MIB, 6 * MIB - 1),
        (12 * MIB, 14 * MIB - 1),
    ];
    assert_eq!(both_copies(&disk), (Some(expected.clone()), Some(expected)));
    assert_eq!(disk.bytes()[..SECTOR as usize], mbr_before[..]);

    let before = original.entries().unwrap();
    let after = GptImage::read(&disk).unwrap().entries().unwrap();
    for (old, new) in before.iter().zip(&after) {
        assert_eq!(
            (
                old.number,
                old.unique_guid,
                old.type_guid,
                &old.name,
                old.attributes
            ),
            (
                new.number,
                new.unique_guid,
                new.type_guid,
                &new.name,
                new.attributes
            )
        );
    }
}

#[test]
fn refuses_to_drop_a_partition_the_layout_does_not_describe() {
    let original = GptImage::read(&disk()).unwrap();
    let without_swap: Vec<Partition> = partitions(&original)
        .into_iter()
        .filter(|p| p.number != 2)
        .collect();

    let result = original.with_partitions(&without_swap, &[]);

    assert!(matches!(result, Err(GptError::UnknownPartition(guid)) if guid == Guid([2; 16])));
}

#[test]
fn removes_and_recreates_swap_in_its_own_slot() {
    let original = GptImage::read(&disk()).unwrap();
    let all = partitions(&original);
    let swap = all[1].clone();
    let others: Vec<Partition> = all.iter().filter(|p| p.number != 2).cloned().collect();

    let removed = original
        .with_partitions(&others, &[swap.unique_guid])
        .unwrap();
    assert_eq!(removed.entries().unwrap().len(), 2);

    let mut recreated_swap = swap.clone();
    recreated_swap.extent.start = Lba(13 * MIB);
    let mut with_swap = others;
    with_swap.push(recreated_swap);
    let recreated = removed.with_partitions(&with_swap, &[]).unwrap();

    let entry = recreated
        .entries()
        .unwrap()
        .into_iter()
        .find(|e| e.number == 2)
        .unwrap();
    assert_eq!(
        (
            entry.unique_guid,
            entry.type_guid,
            entry.name.as_str(),
            entry.extent.start
        ),
        (swap.unique_guid, swap.type_guid, "swap", Lba(13 * MIB))
    );
}

#[test]
fn refuses_to_change_a_partitions_slot() {
    let original = GptImage::read(&disk()).unwrap();
    let mut changed = partitions(&original);
    changed[2].number = 7;

    assert!(matches!(
        original.with_partitions(&changed, &[]),
        Err(GptError::IdentityChanged(_))
    ));
}

#[test]
fn refuses_overlapping_partitions() {
    let original = GptImage::read(&disk()).unwrap();
    let overlapping = moved(partitions(&original), 3, 5);

    assert!(matches!(
        original.with_partitions(&overlapping, &[]),
        Err(GptError::Unreadable(_))
    ));
}

#[test]
fn reads_the_backup_when_the_primary_is_damaged() {
    let mut bytes = disk().into_bytes();
    bytes[SECTOR as usize..2 * SECTOR as usize].fill(0xEE);
    let damaged = MemDisk::from_bytes(bytes, SECTOR as u32).unwrap();

    let entries = GptImage::read(&damaged).unwrap().entries().unwrap();

    assert_eq!(entries.len(), 3);
}

#[test]
fn every_crash_while_writing_leaves_a_readable_table_and_resuming_completes_it() {
    let disk = disk();
    let original = GptImage::read(&disk).unwrap();
    let target_partitions = moved(partitions(&original), 3, 12);
    let target = original.with_partitions(&target_partitions, &[]).unwrap();
    let old_entries = original.entries().unwrap();
    let new_entries = target.entries().unwrap();
    let mut finished = disk.clone();
    target.write(&mut finished).unwrap();
    let new_extents = both_copies(&finished);

    let mut recorded = CrashDisk::new(disk);
    target.write(&mut recorded).unwrap();

    let outcomes = [PendingWrites::Lost, PendingWrites::AllLanded]
        .into_iter()
        .chain((0..32).map(|seed| PendingWrites::Scrambled { seed }));
    for outcome in outcomes {
        for point in recorded.crash_points() {
            let mut image = recorded.crash_image(point, outcome);
            let survivor = GptImage::read(&image).unwrap();
            let read = survivor.entries().unwrap();
            assert!(
                read == old_entries || read == new_entries,
                "point {point}, {outcome:?}: table is neither old nor new"
            );

            let rebuilt = survivor.with_partitions(&target_partitions, &[]).unwrap();
            rebuilt.write(&mut image).unwrap();
            assert_eq!(
                both_copies(&image),
                new_extents,
                "point {point}, {outcome:?}"
            );
            assert_eq!(
                image.bytes()[..SECTOR as usize],
                finished.bytes()[..SECTOR as usize]
            );
        }
    }
}
