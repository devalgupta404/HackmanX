//! `RealDevice` on disk-image files. The images live in Cargo's target directory, because
//! tmpfs (often `/tmp`) doesn't support `O_DIRECT`.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use bootshift_disk::{Access, BlockDevice, DiskError, RealDevice};
use tempfile::TempDir;

const MIB: usize = 1 << 20;

fn image(len: usize) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let path = dir.path().join("disk.img");
    std::fs::write(&path, vec![0u8; len]).unwrap();
    (dir, path)
}

fn open(path: &Path) -> RealDevice {
    RealDevice::open_image(path, Access::ReadWrite, 512).unwrap()
}

#[test]
fn reports_size_and_block_size() {
    let (_dir, path) = image(4 * MIB);
    let device = open(&path);
    assert_eq!(device.len_bytes(), 4 * MIB as u64);
    assert_eq!(device.logical_block_size(), 512);
}

#[test]
fn written_data_is_read_back_and_survives_reopening() {
    let (_dir, path) = image(4 * MIB);
    let data: Vec<u8> = (0..3 * 512).map(|i| (i % 251) as u8).collect();
    {
        let mut device = open(&path);
        device.write_at(MIB as u64 + 512, &data).unwrap();
        device.flush().unwrap();
    }

    let device = open(&path);
    let mut back = vec![0; data.len()];
    device.read_at(MIB as u64 + 512, &mut back).unwrap();

    assert_eq!(back, data);
}

#[test]
fn transfers_larger_than_the_bounce_buffer_are_split() {
    let (_dir, path) = image(20 * MIB);
    let data: Vec<u8> = (0..18 * MIB).map(|i| (i % 253) as u8).collect();
    let mut device = open(&path);

    device.write_at(512, &data).unwrap();
    let mut back = vec![0; data.len()];
    device.read_at(512, &mut back).unwrap();

    assert!(back == data);
}

#[test]
fn misaligned_and_out_of_bounds_access_is_refused() {
    let (_dir, path) = image(MIB);
    let mut device = open(&path);
    let mut buf = vec![0; 512];

    assert!(matches!(
        device.read_at(100, &mut buf),
        Err(DiskError::Misaligned { .. })
    ));
    assert!(matches!(
        device.write_at(MIB as u64, &buf),
        Err(DiskError::OutOfBounds { .. })
    ));
}

#[test]
fn read_only_device_cannot_write() {
    let (_dir, path) = image(MIB);
    let mut device = RealDevice::open_image(&path, Access::ReadOnly, 512).unwrap();

    assert!(matches!(
        device.write_at(0, &[1; 512]),
        Err(DiskError::Io { .. })
    ));
}

#[test]
fn refuses_an_image_that_is_not_whole_blocks() {
    let (_dir, path) = image(1000);
    assert!(matches!(
        RealDevice::open_image(&path, Access::ReadOnly, 512),
        Err(DiskError::InvalidGeometry { .. })
    ));
}

#[test]
fn open_refuses_a_regular_file_as_a_block_device() {
    let (_dir, path) = image(MIB);
    assert!(matches!(
        RealDevice::open(&path, Access::ReadOnly),
        Err(DiskError::Open { .. })
    ));
}
