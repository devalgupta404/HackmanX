//! Building a real recovery initramfs from this machine's tools and modules (no root needed).

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use bootshift_boot::initramfs::{self, Inputs, MODULE_LIST_PATH};
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};

fn search_path() -> Vec<PathBuf> {
    [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

fn kernel_release() -> String {
    let output = SystemRunner
        .run_ok(&CommandLine::new("uname").arg("-r"))
        .unwrap();
    output.text().trim().to_string()
}

#[test]
fn builds_an_archive_with_init_tools_libraries_and_module_indexes() {
    let release = kernel_release();
    let search_path = search_path();
    let init = initramfs::find_program("true", &search_path).unwrap();
    let inputs = Inputs {
        init: &init,
        kernel_release: &release,
        search_path: &search_path,
    };

    let archive = initramfs::build_archive(&SystemRunner, &inputs).unwrap();

    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let archive_path = dir.path().join("initrd.cpio");
    std::fs::write(&archive_path, &archive).unwrap();
    let listing = SystemRunner
        .run_ok(
            &CommandLine::new("cpio")
                .arg("-it")
                .arg("-F")
                .path(&archive_path),
        )
        .unwrap()
        .text()
        .into_owned();
    let names: Vec<&str> = listing.lines().collect();
    for expected in [
        "init",
        "dev/console",
        MODULE_LIST_PATH.trim_start_matches('/'),
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    assert!(names.iter().any(|n| n.ends_with("/e2fsck")));
    assert!(names.iter().any(|n| n.contains("libc.so")));
    assert!(names.contains(&format!("lib/modules/{release}/modules.dep").as_str()));

    let compressed = dir.path().join("initrd.img");
    initramfs::compress(&SystemRunner, &archive_path, &compressed).unwrap();
    let size = std::fs::metadata(&compressed).unwrap().len();
    println!(
        "initramfs: {} KiB uncompressed, {} KiB with zstd",
        archive.len() >> 10,
        size >> 10
    );
    assert!(size < archive.len() as u64);
}
