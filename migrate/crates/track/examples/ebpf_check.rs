//! A live check of the eBPF tracker, run as root on a Linux box:
//!   cargo build -p bootshift-migrate-track --features ebpf --example ebpf_check
//!   sudo ./target/debug/examples/ebpf_check
//! It sets up a loop device, attaches the tracker, writes two known regions with O_DIRECT, then
//! flips and drains and checks the drained ranges cover exactly those regions.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[cfg(not(feature = "ebpf"))]
fn main() {
    eprintln!("build with --features ebpf");
    std::process::exit(2);
}

#[cfg(feature = "ebpf")]
fn main() {
    use std::path::Path;
    use std::process::Command;

    use bootshift_migrate_core::DiskRange;
    use bootshift_migrate_track::{Drained, EbpfTracker, Tracker};

    const SIZE: u64 = 64 << 20;
    let img = "/tmp/ebpf-check.img";

    run(Command::new("truncate").args(["-s", "64M", img]));
    let dev = String::from_utf8(
        Command::new("losetup")
            .args(["--find", "--show", img])
            .output()
            .expect("losetup")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    println!("loop device: {dev}");

    let result = std::panic::catch_unwind(|| {
        let mut tracker = EbpfTracker::open(Path::new(&dev), SIZE).expect("open tracker");
        tracker.confirm_live().expect("confirm_live");

        // Two writes, in the current (generation 0) window: 8 KiB at 1 MiB and at 5 MiB.
        write_direct(&dev, 256, 2); // block 256 = 1 MiB, 2 blocks = 8 KiB
        write_direct(&dev, 1280, 2); // block 1280 = 5 MiB

        // Flip past generation 0, then drain it.
        let generation = tracker.flip().expect("flip");
        let drained = tracker.drain(generation).expect("drain");

        let ranges = match drained {
            Drained::Ranges(ranges) => ranges,
            Drained::Broken(reason) => panic!("unexpected broken tracking: {reason:?}"),
        };
        println!("drained ranges: {ranges:?}");

        let want_a = DiskRange::new(1 << 20, (1 << 20) + 8192);
        let want_b = DiskRange::new(5 << 20, (5 << 20) + 8192);
        assert!(
            covers(&ranges, want_a),
            "the 1 MiB write was not tracked: {ranges:?}"
        );
        assert!(
            covers(&ranges, want_b),
            "the 5 MiB write was not tracked: {ranges:?}"
        );

        // A second drain of the same generation is empty: the first one cleared it.
        let generation2 = tracker.flip().expect("flip 2");
        let again = tracker.drain(generation2).expect("drain 2");
        assert_eq!(
            again,
            Drained::Ranges(vec![]),
            "the buffer was not cleared after draining"
        );
    });

    let _ = Command::new("losetup").args(["-d", &dev]).status();
    let _ = std::fs::remove_file(img);

    match result {
        Ok(()) => println!("EBPF TRACKER CHECK PASSED"),
        Err(_) => {
            eprintln!("EBPF TRACKER CHECK FAILED");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "ebpf")]
fn covers(ranges: &[bootshift_migrate_core::DiskRange], want: bootshift_migrate_core::DiskRange) -> bool {
    ranges
        .iter()
        .any(|r| r.start <= want.start && r.end >= want.end)
}

#[cfg(feature = "ebpf")]
fn write_direct(dev: &str, seek_blocks: u64, count_blocks: u64) {
    use std::process::Command;
    run(Command::new("dd").args([
        "if=/dev/urandom",
        &format!("of={dev}"),
        "bs=4096",
        &format!("seek={seek_blocks}"),
        &format!("count={count_blocks}"),
        "oflag=direct",
        "conv=notrunc",
        "status=none",
    ]));
}

#[cfg(feature = "ebpf")]
fn run(command: &mut std::process::Command) {
    let status = command.status().expect("running a command");
    assert!(status.success(), "{command:?} failed with {status}");
}
