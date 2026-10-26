//! Real ext4 and NTFS filesystems, moved inside a disk image by the copier using only the
//! ranges their own tools report as used. Afterwards the filesystem must check clean and every
//! file must read back unchanged, including after power cuts during the move.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use bootshift_core::journal::{self, PlanRecord, Progress, Stage};
use bootshift_disk::{
    Access, BlockDevice, CrashDisk, FileExtent, Journal, JournalFile, MemDisk, PendingWrites,
    RealDevice,
};
use bootshift_engine::copier::{CopyOptions, CopyScope, MoveJob, move_range};
use bootshift_fs::ntfs::DirtyMark;
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};
use bootshift_fs::{ext4, ntfs};
use tempfile::TempDir;

const MIB: u64 = 1 << 20;
const BLOCK: u32 = 512;
const FS_LEN: u64 = 64 * MIB;
const FILES: [(&str, u64); 3] = [("a", MIB / 3), ("b", 4 * MIB), ("c", 9 * MIB)];

#[derive(Clone, Copy)]
enum Kind {
    Ext4,
    Ntfs,
}

fn contents(name: &str, len: u64) -> Vec<u8> {
    let seed = u64::from(name.as_bytes()[0]);
    (0..len)
        .map(|i| (i.wrapping_mul(seed) >> 5) as u8)
        .collect()
}

fn run(command: CommandLine) -> Vec<u8> {
    SystemRunner.run_ok(&command).unwrap().stdout
}

fn make_filesystem(kind: Kind, dir: &Path) -> PathBuf {
    let files = dir.join("files");
    std::fs::create_dir_all(&files).unwrap();
    for (name, len) in FILES {
        std::fs::write(files.join(name), contents(name, len)).unwrap();
    }
    let image = dir.join("fs.img");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(FS_LEN)
        .unwrap();
    match kind {
        Kind::Ext4 => {
            run(CommandLine::new("mkfs.ext4")
                .arg("-q")
                .arg("-F")
                .arg("-d")
                .path(&files)
                .path(&image));
        }
        Kind::Ntfs => {
            run(CommandLine::new("mkfs.ntfs")
                .arg("-q")
                .arg("-F")
                .arg("-f")
                .path(&image));
            for (name, _) in FILES {
                run(CommandLine::new("ntfscp")
                    .path(&image)
                    .path(&files.join(name))
                    .arg(format!("/{name}")));
            }
        }
    }
    image
}

fn used_ranges(kind: Kind, image: &Path) -> Vec<std::ops::Range<u64>> {
    match kind {
        Kind::Ext4 => ext4::used_ranges(&SystemRunner, image).unwrap(),
        Kind::Ntfs => {
            let info = ntfs::info(&SystemRunner, image, DirtyMark::Refuse).unwrap();
            ntfs::used_ranges(&SystemRunner, image, &info, DirtyMark::Refuse).unwrap()
        }
    }
}

fn assert_filesystem_intact(kind: Kind, image: &Path, dir: &Path, context: &str) {
    match kind {
        Kind::Ext4 => {
            ext4::check(&SystemRunner, image).unwrap();
            for (name, len) in FILES {
                let out = dir.join(format!("{name}.out"));
                run(CommandLine::new("debugfs")
                    .arg("-R")
                    .arg(format!("dump /{name} {}", out.display()))
                    .path(image));
                assert!(
                    std::fs::read(out).unwrap() == contents(name, len),
                    "{context}: {name}"
                );
            }
        }
        Kind::Ntfs => {
            ntfs::info(&SystemRunner, image, DirtyMark::Refuse).unwrap();
            for (name, len) in FILES {
                let read = run(CommandLine::new("ntfscat")
                    .path(image)
                    .arg(format!("/{name}")));
                assert!(read == contents(name, len), "{context}: {name}");
            }
        }
    }
}

struct Setup {
    dir: TempDir,
    disk: MemDisk,
    job: MoveJob,
}

/// A disk with the journal at the start and the filesystem at `source_mib`, to be moved to
/// `destination_mib`.
fn setup(kind: Kind, source_mib: u64, destination_mib: u64) -> Setup {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let fs_image = make_filesystem(kind, dir.path());
    let ranges = used_ranges(kind, &fs_image);

    let disk_len = journal::FILE_LEN + (source_mib.max(destination_mib) * MIB) + FS_LEN;
    let mut bytes = vec![0; disk_len as usize];
    let plan = PlanRecord {
        run_id: 1,
        payload: Vec::new(),
    };
    let journal_bytes = journal::initial_file_contents(&plan);
    bytes[..journal_bytes.len()].copy_from_slice(&journal_bytes);
    let source = journal::FILE_LEN + source_mib * MIB;
    bytes[source as usize..(source + FS_LEN) as usize]
        .copy_from_slice(&std::fs::read(&fs_image).unwrap());

    Setup {
        dir,
        disk: MemDisk::from_bytes(bytes, BLOCK).unwrap(),
        job: MoveJob {
            source,
            destination: journal::FILE_LEN + destination_mib * MIB,
            len: FS_LEN,
            scope: CopyScope::used_only(FS_LEN, ranges),
        },
    }
}

fn journal_file() -> JournalFile {
    JournalFile::new(vec![FileExtent {
        file_offset: 0,
        device_offset: 0,
        len: journal::FILE_LEN,
    }])
    .unwrap()
}

fn resume_to_end<D: BlockDevice>(disk: &mut D, job: &MoveJob) {
    let opened = Journal::open(disk, journal_file()).unwrap();
    let start = opened.progress.unwrap_or(Progress {
        stage: Stage::Running,
        operation: 0,
        steps_done: 0,
        next_chunk: 0,
        hash_chain: [0; 32],
    });
    let mut journal = opened.journal;
    move_range(
        disk,
        &mut journal,
        job.clone(),
        &start,
        CopyOptions::default(),
    )
    .unwrap();
}

fn extract(disk: &MemDisk, job: &MoveJob, path: &Path) {
    let start = job.destination as usize;
    std::fs::write(path, &disk.bytes()[start..start + job.len as usize]).unwrap();
}

fn assert_moves_intact(kind: Kind, source_mib: u64, destination_mib: u64) {
    let Setup { dir, mut disk, job } = setup(kind, source_mib, destination_mib);
    resume_to_end(&mut disk, &job);
    let moved = dir.path().join("moved.img");
    extract(&disk, &job, &moved);
    assert_filesystem_intact(kind, &moved, dir.path(), "after the move");
}

fn assert_survives_sampled_crashes(kind: Kind, source_mib: u64, destination_mib: u64) {
    let Setup { dir, disk, job } = setup(kind, source_mib, destination_mib);
    let mut recorded = CrashDisk::new(disk);
    resume_to_end(&mut recorded, &job);

    let last = *recorded.crash_points().end();
    for (sample, point) in (0..=last).step_by((last / 6).max(1)).enumerate() {
        let outcome = PendingWrites::Scrambled {
            seed: sample as u64,
        };
        let mut image = recorded.crash_image(point, outcome);
        resume_to_end(&mut image, &job);
        let moved = dir.path().join("moved.img");
        extract(&image, &job, &moved);
        assert_filesystem_intact(kind, &moved, dir.path(), &format!("crash at point {point}"));
    }
}

#[test]
fn ext4_moved_left_into_overlapping_space_stays_intact() {
    assert_moves_intact(Kind::Ext4, 3, 0);
}

#[test]
fn ext4_moved_right_stays_intact() {
    assert_moves_intact(Kind::Ext4, 0, 10);
}

#[test]
fn ntfs_moved_left_stays_intact() {
    assert_moves_intact(Kind::Ntfs, 5, 0);
}

#[test]
fn ext4_survives_power_cuts_during_the_move() {
    assert_survives_sampled_crashes(Kind::Ext4, 0, 7);
}

#[test]
fn ntfs_survives_power_cuts_during_the_move() {
    assert_survives_sampled_crashes(Kind::Ntfs, 6, 0);
}

#[test]
fn moving_on_a_real_image_file_through_o_direct_works_too() {
    let Setup { dir, disk, job } = setup(Kind::Ext4, 2, 0);
    let disk_path = dir.path().join("disk.img");
    std::fs::write(&disk_path, disk.bytes()).unwrap();

    let mut device = RealDevice::open_image(&disk_path, Access::ReadWrite, BLOCK).unwrap();
    resume_to_end(&mut device, &job);
    let mut bytes = vec![0; device.len_bytes() as usize];
    device.read_at(0, &mut bytes).unwrap();

    let moved = dir.path().join("moved.img");
    extract(&MemDisk::from_bytes(bytes, BLOCK).unwrap(), &job, &moved);
    assert_filesystem_intact(Kind::Ext4, &moved, dir.path(), "after an O_DIRECT move");
}
