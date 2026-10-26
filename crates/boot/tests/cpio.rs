//! Archives from `CpioWriter` must unpack with the system's own `cpio`.

#![allow(clippy::unwrap_used)]

use bootshift_boot::cpio::CpioWriter;
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};

#[test]
fn system_cpio_unpacks_directories_files_and_symlinks() {
    let mut writer = CpioWriter::new();
    writer.directory("/usr", 0o755);
    writer.directory("/usr/bin", 0o755);
    writer.file("/usr/bin/tool", 0o755, b"#!/bin/true\n");
    writer.file("/etc-note", 0o644, &[7u8; 1001]);
    writer.symlink("/bin", "usr/bin");
    let archive = writer.finish();

    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let archive_path = dir.path().join("test.cpio");
    std::fs::write(&archive_path, &archive).unwrap();
    let unpacked = dir.path().join("root");
    std::fs::create_dir(&unpacked).unwrap();

    let list = SystemRunner
        .run_ok(
            &CommandLine::new("cpio")
                .arg("-it")
                .arg("-F")
                .path(&archive_path),
        )
        .unwrap();
    assert_eq!(
        list.text().lines().collect::<Vec<_>>(),
        ["usr", "usr/bin", "usr/bin/tool", "etc-note", "bin"]
    );

    SystemRunner
        .run_ok(
            &CommandLine::new("cpio")
                .arg("-idm")
                .arg("-D")
                .path(&unpacked)
                .arg("-F")
                .path(&archive_path),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(unpacked.join("usr/bin/tool")).unwrap(),
        b"#!/bin/true\n"
    );
    assert_eq!(
        std::fs::read(unpacked.join("etc-note")).unwrap(),
        vec![7u8; 1001]
    );
    assert_eq!(
        std::fs::read_link(unpacked.join("bin")).unwrap().to_str(),
        Some("usr/bin")
    );
}
