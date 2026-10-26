//! The executor running whole plans on a disk with a real GPT, including a power cut at every
//! write and flush followed by a resume from the journal alone.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;

use bootshift_core::journal::{self, Stage};
use bootshift_core::layout::{Content, Extent, Geometry, Guid, Layout, Partition, Target};
use bootshift_core::planner::plan;
use bootshift_core::scope::CopyScope;
use bootshift_core::units::{Lba, Sectors};
use bootshift_disk::{
    BlockDevice, CrashDisk, FileExtent, GptImage, Journal, JournalFile, MemDisk, PendingWrites,
};
use bootshift_engine::copier::CopyOptions;
use bootshift_engine::executor::{Filesystems, execute};
use bootshift_engine::run_plan::RunPlan;
use bootshift_fs::run::RunError;
use gptman::{GPT, GPTPartitionEntry};

const SECTOR: u64 = 512;
const DISK_SECTORS: u64 = 11_000;
const JOURNAL_AT: u64 = 2048 * SECTOR;
const ESP: (u32, u64, u64) = (1, 2048, 8448);
const WINDOWS: (u32, u64, u64) = (2, 10_496, 64);
const LINUX: (u32, u64, u64) = (3, 10_560, 64);
const SWAP: (u32, u64, u64) = (4, 10_624, 16);

fn type_guid(content: Content) -> [u8; 16] {
    match content {
        Content::EfiSystem => Guid::EFI_SYSTEM.0,
        Content::Swap => [0x6D; 16],
        Content::Ntfs => [0xA2; 16],
        _ => [0xAF; 16],
    }
}

fn partitions() -> Vec<(u32, u64, u64, Content)> {
    vec![
        (ESP.0, ESP.1, ESP.2, Content::EfiSystem),
        (WINDOWS.0, WINDOWS.1, WINDOWS.2, Content::Ntfs),
        (LINUX.0, LINUX.1, LINUX.2, Content::Ext4),
        (SWAP.0, SWAP.1, SWAP.2, Content::Swap),
    ]
}

/// Byte `offset` of partition `number`'s original contents.
fn pattern(number: u32, offset: u64) -> u8 {
    (offset.wrapping_mul(u64::from(number) * 37 + 11) >> 3) as u8 ^ number as u8
}

fn layout() -> Layout {
    let parts = partitions()
        .into_iter()
        .map(|(number, start, len, content)| Partition {
            number,
            unique_guid: Guid([number as u8 + 0x10; 16]),
            type_guid: Guid(type_guid(content)),
            name: format!("part{number}"),
            attributes: 0,
            extent: Extent::new(Lba(start), Sectors(len)),
            content,
            used: Sectors(len / 2),
            min_len: Some(Sectors(len / 2)),
        })
        .collect();
    Layout {
        geometry: Geometry {
            sector_size: SECTOR as u32,
            usable: Extent::new(Lba(34), Sectors(DISK_SECTORS - 34 - 33)),
            alignment: Sectors(8),
        },
        partitions: parts,
    }
}

/// Shrink Windows, move Linux left into the space and grow it over swap's old place, and
/// recreate swap after it.
fn target(layout: &Layout) -> Target {
    let mut target: Target = layout
        .partitions
        .iter()
        .map(|p| (p.unique_guid, p.extent))
        .collect();
    let guid = |number: u32| Guid([number as u8 + 0x10; 16]);
    target.insert(guid(2), Extent::new(Lba(10_496), Sectors(48)));
    target.insert(guid(3), Extent::new(Lba(10_544), Sectors(96)));
    target.insert(guid(4), Extent::new(Lba(10_640), Sectors(16)));
    target
}

fn journal_file() -> JournalFile {
    JournalFile::new(vec![FileExtent {
        file_offset: 0,
        device_offset: JOURNAL_AT,
        len: journal::FILE_LEN,
    }])
    .unwrap()
}

fn run_plan() -> RunPlan {
    let layout = layout();
    let plan = plan(&layout, &target(&layout)).unwrap();
    RunPlan { layout, plan }
}

fn fresh_disk(run: &RunPlan) -> MemDisk {
    let mut cursor = Cursor::new(vec![0; (DISK_SECTORS * SECTOR) as usize]);
    let mut gpt = GPT::new_from(&mut cursor, SECTOR, [0xD1; 16]).unwrap();
    for partition in &run.layout.partitions {
        gpt[partition.number] = GPTPartitionEntry {
            partition_type_guid: partition.type_guid.0,
            unique_partition_guid: partition.unique_guid.0,
            starting_lba: partition.extent.start.0,
            ending_lba: partition.extent.end().0 - 1,
            attribute_bits: 0,
            partition_name: partition.name.as_str().into(),
        };
    }
    gpt.write_into(&mut cursor).unwrap();
    let mut bytes = cursor.into_inner();

    for (number, start, len, content) in partitions() {
        if content != Content::EfiSystem {
            for offset in 0..len * SECTOR {
                bytes[(start * SECTOR + offset) as usize] = pattern(number, offset);
            }
        }
    }
    let journal = journal::initial_file_contents(&run.to_record(42).unwrap());
    bytes[JOURNAL_AT as usize..JOURNAL_AT as usize + journal.len()].copy_from_slice(&journal);
    MemDisk::from_bytes(bytes, SECTOR as u32).unwrap()
}

/// Filesystem tools that only record what they were asked to do.
#[derive(Default)]
struct FakeFilesystems {
    calls: Vec<String>,
}

impl Filesystems for FakeFilesystems {
    fn used_ranges(&mut self, partition: &Partition) -> Result<CopyScope, RunError> {
        self.calls.push(format!("used {}", partition.number));
        let len = partition.extent.len.0 * SECTOR;
        Ok(CopyScope::UsedOnly(vec![0..len / 2]))
    }
    fn shrink(&mut self, partition: &Partition, len: u64) -> Result<(), RunError> {
        self.calls
            .push(format!("shrink {} to {len}", partition.number));
        Ok(())
    }
    fn grow(&mut self, partition: &Partition) -> Result<(), RunError> {
        self.calls.push(format!(
            "grow {} to {}",
            partition.number, partition.extent.len.0
        ));
        Ok(())
    }
    fn recreate_swap(&mut self, partition: &Partition) -> Result<(), RunError> {
        self.calls.push(format!(
            "mkswap {} at {}",
            partition.number, partition.extent.start.0
        ));
        Ok(())
    }
    fn check(&mut self, partition: &Partition) -> Result<(), RunError> {
        self.calls.push(format!("check {}", partition.number));
        Ok(())
    }
    fn refresh_kernel_view(&mut self) -> Result<(), RunError> {
        Ok(())
    }
}

/// Opens the journal on `disk` and runs the plan from wherever it got to.
fn resume<D: BlockDevice>(disk: &mut D) -> FakeFilesystems {
    let opened = Journal::open(disk, journal_file()).unwrap();
    let run = RunPlan::from_record(&opened.plan).unwrap();
    let mut journal = opened.journal;
    let mut filesystems = FakeFilesystems::default();
    execute(
        disk,
        &mut journal,
        &mut filesystems,
        &run,
        opened.progress,
        &CopyOptions::default(),
    )
    .unwrap();
    filesystems
}

fn assert_finished(disk: &MemDisk, context: &str) {
    let run = run_plan();
    let target = target(&run.layout);
    let entries = GptImage::read(disk).unwrap().entries().unwrap();
    let extents: BTreeMap<Guid, Extent> =
        entries.iter().map(|e| (e.unique_guid, e.extent)).collect();
    assert_eq!(extents, target, "{context}: table is not the target");

    for partition in &run.layout.partitions {
        if !partition.content.copies_used_only() {
            continue;
        }
        let new = target[&partition.unique_guid];
        let kept = partition.extent.len.0.min(new.len.0) * SECTOR / 2;
        for offset in 0..kept {
            let byte = disk.bytes()[(new.start.0 * SECTOR + offset) as usize];
            assert_eq!(
                byte,
                pattern(partition.number, offset),
                "{context}: partition {} byte {offset} was not moved intact",
                partition.number
            );
        }
    }

    let progress = Journal::open(disk, journal_file())
        .unwrap()
        .progress
        .unwrap();
    assert_eq!(progress.stage, Stage::Finishing, "{context}");
}

#[test]
fn the_plan_shrinks_moves_grows_and_recreates_swap() {
    let ops = run_plan().plan.operations;
    let names: Vec<&str> = ops
        .iter()
        .map(|op| match op {
            bootshift_core::plan::Operation::Shrink { .. } => "shrink",
            bootshift_core::plan::Operation::Move { .. } => "move",
            bootshift_core::plan::Operation::Grow { .. } => "grow",
            bootshift_core::plan::Operation::RemoveSwap { .. } => "remove swap",
            bootshift_core::plan::Operation::RecreateSwap { .. } => "recreate swap",
        })
        .collect();
    assert_eq!(
        names,
        ["remove swap", "shrink", "move", "grow", "recreate swap"]
    );
}

#[test]
fn runs_a_whole_plan_and_calls_each_tool_in_order() {
    let mut disk = fresh_disk(&run_plan());

    let filesystems = resume(&mut disk);

    assert_finished(&disk, "uninterrupted run");
    assert_eq!(
        filesystems.calls,
        [
            "shrink 2 to 24576",
            "used 3",
            "grow 3 to 96",
            "mkswap 4 at 10640",
            "check 2",
            "check 3",
        ]
    );
}

#[test]
fn a_finished_run_resumes_to_nothing() {
    let mut disk = fresh_disk(&run_plan());
    resume(&mut disk);
    let before = disk.clone();

    let filesystems = resume(&mut disk);

    assert!(filesystems.calls.is_empty());
    assert!(disk == before);
}

#[test]
fn every_crash_during_a_run_resumes_to_the_same_result() {
    let mut recorded = CrashDisk::new(fresh_disk(&run_plan()));
    resume(&mut recorded);

    let outcomes = [PendingWrites::Lost, PendingWrites::AllLanded]
        .into_iter()
        .chain((0..4).map(|seed| PendingWrites::Scrambled { seed }));
    for outcome in outcomes {
        for point in recorded.crash_points() {
            let mut image = recorded.crash_image(point, outcome);
            resume(&mut image);
            assert_finished(&image, &format!("crash at point {point} with {outcome:?}"));
        }
    }
}

#[test]
fn a_second_crash_during_recovery_also_resumes_to_the_same_result() {
    let mut recorded = CrashDisk::new(fresh_disk(&run_plan()));
    resume(&mut recorded);

    let last = *recorded.crash_points().end();
    for (sample, point) in (0..=last).step_by(5).enumerate() {
        let first = recorded.crash_image(
            point,
            PendingWrites::Scrambled {
                seed: sample as u64,
            },
        );
        let mut recovering = CrashDisk::new(first);
        resume(&mut recovering);

        let second_point = *recovering.crash_points().end() / 2;
        let outcome = PendingWrites::Scrambled {
            seed: 1000 + sample as u64,
        };
        let mut image = recovering.crash_image(second_point, outcome);
        resume(&mut image);
        assert_finished(
            &image,
            &format!("crash at {point}, then again at {second_point}"),
        );
    }
}
