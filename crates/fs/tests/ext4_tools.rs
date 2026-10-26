//! The ext4 adapter against the real e2fsprogs, on image files (no root needed).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use bootshift_fs::ext4;
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};
use tempfile::TempDir;

const MIB: u64 = 1 << 20;
const FILES: [(&str, u64); 3] = [("small", MIB / 3), ("medium", 3 * MIB), ("large", 7 * MIB)];

fn contents(name: &str, len: u64) -> Vec<u8> {
    let seed = name.len() as u64;
    (0..len)
        .map(|i| (i.wrapping_mul(seed + 31) >> 3) as u8)
        .collect()
}

/// A 64 MiB ext4 image holding the test files.
fn image() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    for (name, len) in FILES {
        std::fs::write(files.join(name), contents(name, len)).unwrap();
    }
    let image = dir.path().join("ext4.img");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(64 * MIB)
        .unwrap();
    let mkfs = CommandLine::new("mkfs.ext4")
        .arg("-q")
        .arg("-F")
        .arg("-d")
        .path(&files)
        .path(&image);
    SystemRunner.run_ok(&mkfs).unwrap();
    (dir, image)
}

fn file_blocks(image: &Path, name: &str) -> Vec<u64> {
    let command = CommandLine::new("debugfs")
        .arg("-R")
        .arg(format!("blocks /{name}"))
        .path(image);
    let output = SystemRunner.run_ok(&command).unwrap();
    output
        .text()
        .split_whitespace()
        .map(|block| block.parse().unwrap())
        .collect()
}

fn read_file(image: &Path, name: &str, scratch: &Path) -> Vec<u8> {
    let out = scratch.join(format!("{name}.out"));
    let command = CommandLine::new("debugfs")
        .arg("-R")
        .arg(format!("dump /{name} {}", out.display()))
        .path(image);
    SystemRunner.run_ok(&command).unwrap();
    std::fs::read(out).unwrap()
}

#[test]
fn summary_and_minimum_size_of_a_fresh_filesystem() {
    let (_dir, image) = image();
    let summary = ext4::summary(&SystemRunner, &image).unwrap();

    assert!(summary.clean);
    assert_eq!(summary.len_bytes(), 64 * MIB);
    let file_bytes: u64 = FILES.iter().map(|(_, len)| len).sum();
    let min_size = ext4::min_size(&SystemRunner, &image, summary.block_size).unwrap();
    assert!(min_size >= file_bytes && min_size < summary.len_bytes());
}

#[test]
fn used_ranges_cover_every_block_of_every_file() {
    let (_dir, image) = image();
    let block_size = ext4::summary(&SystemRunner, &image).unwrap().block_size;
    let used = ext4::used_ranges(&SystemRunner, &image).unwrap();

    for (name, _) in FILES {
        for block in file_blocks(&image, name) {
            let offset = block * block_size;
            assert!(
                used.iter().any(|r| r.contains(&offset)),
                "block {block} of {name} is not in the used ranges"
            );
        }
    }
}

#[test]
fn check_passes_on_a_clean_filesystem() {
    let (_dir, image) = image();
    assert!(ext4::check(&SystemRunner, &image).is_ok());
}

#[test]
fn offline_shrink_keeps_every_file_intact() {
    let (dir, image) = image();

    ext4::resize_offline(&SystemRunner, &image, 40 * MIB).unwrap();

    let summary = ext4::summary(&SystemRunner, &image).unwrap();
    assert_eq!(summary.len_bytes(), 40 * MIB);
    ext4::check(&SystemRunner, &image).unwrap();
    for (name, len) in FILES {
        assert!(
            read_file(&image, name, dir.path()) == contents(name, len),
            "{name} changed"
        );
    }
}
