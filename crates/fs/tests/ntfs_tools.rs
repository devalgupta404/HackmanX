//! The NTFS adapter against the real ntfs-3g tools, on image files (no root needed). Files
//! are put in with `ntfscp` and read back with `ntfscat`, so nothing is mounted.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use bootshift_fs::ntfs::{self, DirtyMark};
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};
use tempfile::TempDir;

const MIB: u64 = 1 << 20;
const FILES: [(&str, u64); 2] = [("notes", MIB / 2), ("photo", 5 * MIB)];

fn contents(name: &str, len: u64) -> Vec<u8> {
    let seed = name.len() as u64;
    (0..len)
        .map(|i| (i.wrapping_mul(seed + 17) >> 2) as u8)
        .collect()
}

/// A 64 MiB NTFS image holding the test files.
fn image() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let image = dir.path().join("ntfs.img");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(64 * MIB)
        .unwrap();
    let mkfs = CommandLine::new("mkfs.ntfs")
        .arg("-q")
        .arg("-F")
        .arg("-f")
        .path(&image);
    SystemRunner.run_ok(&mkfs).unwrap();
    for (name, len) in FILES {
        let source = dir.path().join(name);
        std::fs::write(&source, contents(name, len)).unwrap();
        let copy = CommandLine::new("ntfscp")
            .path(&image)
            .path(&source)
            .arg(format!("/{name}"));
        SystemRunner.run_ok(&copy).unwrap();
    }
    (dir, image)
}

fn read_file(image: &Path, name: &str) -> Vec<u8> {
    let command = CommandLine::new("ntfscat")
        .arg("--force")
        .path(image)
        .arg(format!("/{name}"));
    SystemRunner.run_ok(&command).unwrap().stdout
}

fn assert_files_intact(image: &Path) {
    for (name, len) in FILES {
        assert!(
            read_file(image, name) == contents(name, len),
            "{name} changed"
        );
    }
}

#[test]
fn info_reads_the_geometry_of_a_clean_volume() {
    let (_dir, image) = image();
    let info = ntfs::info(&SystemRunner, &image, DirtyMark::Refuse).unwrap();

    assert_eq!(info.cluster_size, 4096);
    assert!(info.volume_size < 64 * MIB && info.volume_size > 63 * MIB);
    let file_bytes: u64 = FILES.iter().map(|(_, len)| len).sum();
    assert!(info.min_size.unwrap() >= file_bytes);
}

#[test]
fn used_ranges_hold_at_least_the_files() {
    let (_dir, image) = image();
    let info = ntfs::info(&SystemRunner, &image, DirtyMark::Refuse).unwrap();
    let used = ntfs::used_ranges(&SystemRunner, &image, &info, DirtyMark::Refuse).unwrap();

    let used_bytes: u64 = used.iter().map(|r| r.end - r.start).sum();
    let file_bytes: u64 = FILES.iter().map(|(_, len)| len).sum();
    assert!(used_bytes >= file_bytes && used_bytes < info.volume_size);
}

#[test]
fn shrink_then_grow_keeps_every_file_intact() {
    let (_dir, image) = image();

    ntfs::resize_offline(&SystemRunner, &image, Some(32 * MIB)).unwrap();
    let shrunk = ntfs::info(&SystemRunner, &image, DirtyMark::ExpectedAfterOurResize).unwrap();
    assert!(shrunk.volume_size <= 32 * MIB);
    assert_files_intact(&image);

    ntfs::resize_offline(&SystemRunner, &image, None).unwrap();
    let grown = ntfs::info(&SystemRunner, &image, DirtyMark::ExpectedAfterOurResize).unwrap();
    assert!(grown.volume_size > 63 * MIB);
    assert_files_intact(&image);
}

#[test]
fn a_volume_marked_for_checking_is_refused_unless_we_marked_it() {
    let (_dir, image) = image();
    ntfs::resize_offline(&SystemRunner, &image, Some(48 * MIB)).unwrap();

    assert!(ntfs::info(&SystemRunner, &image, DirtyMark::Refuse).is_err());
    assert!(ntfs::info(&SystemRunner, &image, DirtyMark::ExpectedAfterOurResize).is_ok());
}
