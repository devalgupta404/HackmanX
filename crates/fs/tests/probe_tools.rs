//! Probing with the real `lsblk`, `blkid` and `mkswap`. Everything here only reads the host's
//! disks, or writes to image files.

#![allow(clippy::unwrap_used)]

use bootshift_fs::probe;
use bootshift_fs::run::{RunError, SystemRunner};
use bootshift_fs::swap;

#[test]
fn lsblk_output_of_this_machine_parses() {
    match probe::disks(&SystemRunner) {
        Ok(_) => {}
        // On lsblk older than 2.37 the fallback reads start sectors with sfdisk, which needs
        // root. An unprivileged run may only fail that way; a parse failure is still a bug.
        Err(RunError::Failed { command, .. }) if command.starts_with("sfdisk") => {}
        Err(error) => panic!("probing this machine failed: {error}"),
    }
}

#[test]
fn recreated_swap_keeps_its_uuid_and_label() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let image = dir.path().join("swap.img");
    std::fs::write(&image, vec![0u8; 16 << 20]).unwrap();
    let uuid = "c0ffee00-1234-4abc-8def-001122334455";

    swap::recreate(&SystemRunner, &image, uuid, Some("myswap")).unwrap();

    let signature = probe::signature(&SystemRunner, &image).unwrap();
    assert_eq!(signature.fstype.as_deref(), Some("swap"));
    assert_eq!(signature.uuid.as_deref(), Some(uuid));
    assert_eq!(signature.label.as_deref(), Some("myswap"));
}

#[test]
fn an_empty_device_has_no_signature() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let image = dir.path().join("empty.img");
    std::fs::write(&image, vec![0u8; 1 << 20]).unwrap();

    assert_eq!(
        probe::signature(&SystemRunner, &image).unwrap(),
        probe::Signature::default()
    );
}
