//! The copier and journal running on a real image file through `O_DIRECT`.

#![allow(clippy::unwrap_used)]

mod common;

use bootshift_disk::{Access, BlockDevice, MemDisk, RealDevice};
use bootshift_engine::copier::{CopyOptions, CopyScope, verify_move};
use common::{BLOCK, destination_holds_source, fresh_disk, job, resume_to_end};

#[test]
fn moves_data_on_an_image_file() {
    let original = fresh_disk();
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let path = dir.path().join("disk.img");
    std::fs::write(&path, original.bytes()).unwrap();
    let job = job(10, 7, 10, CopyScope::Everything);
    let options = CopyOptions::default();

    let mut device = RealDevice::open_image(&path, Access::ReadWrite, BLOCK as u32).unwrap();
    let chain = resume_to_end(&mut device, &job, &options);

    assert_eq!(verify_move(&device, job.clone(), &options).unwrap(), chain);
    let mut bytes = vec![0; device.len_bytes() as usize];
    device.read_at(0, &mut bytes).unwrap();
    let moved = MemDisk::from_bytes(bytes, BLOCK as u32).unwrap();
    assert!(destination_holds_source(&original, &moved, &job));
}
