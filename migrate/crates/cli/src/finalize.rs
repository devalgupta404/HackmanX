//! `finalize`: after the data is migrated, make the target disk whole on its new, larger hardware
//! (implementation I6). It moves the GPT's backup header to the real end of the disk, grows the
//! last partition to fill the new space, and grows its filesystem into it (the reshape USP) — then
//! adapts the guest's config for its new device names (M9 keeps every UUID, so only bare `/dev`
//! references change). Growing only ever adds space, so it is safe after the copy (data first,
//! table last); the heavy lifting is done by the standard, well-tested tools.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use bootshift_migrate_core::rewrite_fstab;

pub fn run(
    data: &Path,
    grow_last: bool,
    adapt_root: Option<&str>,
    renames: &[(String, String)],
) -> Result<()> {
    let disk = data.to_string_lossy().into_owned();
    if grow_last {
        reshape_last_partition(&disk)?;
    }
    if let Some(mount) = adapt_root {
        adapt_root_fstab(mount, renames)?;
    }
    println!("finalize: done");
    Ok(())
}

/// Grows the last partition of `disk`, and its filesystem, to fill the disk.
fn reshape_last_partition(disk: &str) -> Result<()> {
    reread_partitions(disk);
    let partition = last_partition_device(disk)?;
    let number = partition_number(&partition)?;
    println!("finalize: growing {partition} to fill {disk}");

    // Move the GPT backup header to the real end of the (now larger) disk.
    accept(Command::new("sgdisk").arg("-e").arg(disk), &[0], "sgdisk -e")?;
    reread_partitions(disk);

    // Extend the partition. growpart exits 0 when it grew it and 2 when there was nothing to grow
    // (already full), both fine for an idempotent step.
    accept(
        Command::new("growpart").arg(disk).arg(number.to_string()),
        &[0, 1, 2],
        "growpart",
    )?;
    reread_partitions(disk);

    // The filesystem must be clean before a resize; e2fsck exits 1 or 2 when it corrected it.
    accept(Command::new("e2fsck").args(["-fy", &partition]), &[0, 1, 2], "e2fsck")?;
    accept(Command::new("resize2fs").arg(&partition), &[0], "resize2fs")?;
    Ok(())
}

/// Rewrites `<mount>/etc/fstab`'s device names for the new hardware, if any changed.
fn adapt_root_fstab(mount: &str, renames: &[(String, String)]) -> Result<()> {
    if renames.is_empty() {
        println!("finalize: no device renames given, fstab left as is");
        return Ok(());
    }
    let path = format!("{mount}/etc/fstab");
    let contents = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    let adapted = rewrite_fstab(&contents, renames);
    if adapted.changes.is_empty() {
        println!("finalize: fstab already references the new devices (nothing to adapt)");
        return Ok(());
    }
    std::fs::write(&path, &adapted.text).with_context(|| format!("writing {path}"))?;
    for change in &adapted.changes {
        println!("finalize: fstab {} -> {}", change.from, change.to);
    }
    Ok(())
}

fn reread_partitions(disk: &str) {
    let _ = Command::new("partprobe").arg(disk).status();
    let _ = Command::new("partx").args(["-u", disk]).status();
}

/// The last partition's kernel device (e.g. `/dev/vdb1`, `/dev/loop5p1`), resolved by `lsblk` so it
/// is correct however `disk` was named, including a `/dev/disk/by-id/...` symlink.
fn last_partition_device(disk: &str) -> Result<String> {
    let output = Command::new("lsblk")
        .args(["-nro", "NAME,TYPE", disk])
        .output()
        .context("running lsblk to list partitions")?;
    if !output.status.success() {
        bail!(
            "lsblk could not read {disk}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let last = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let kind = fields.next()?;
            (kind == "part").then(|| name.to_string())
        })
        .next_back()
        .with_context(|| format!("{disk} has no partitions to finalize"))?;
    Ok(format!("/dev/{last}"))
}

/// The partition number from a partition device name: the trailing digits (`vdb1` → 1,
/// `nvme0n1p2` → 2, `loop5p1` → 1).
fn partition_number(partition: &str) -> Result<u32> {
    let digits: String = partition
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse()
        .with_context(|| format!("no partition number in {partition}"))
}

/// Runs a tool, accepting any of `ok_codes` as success so idempotent re-runs don't fail.
fn accept(command: &mut Command, ok_codes: &[i32], label: &str) -> Result<()> {
    let output = command.output().with_context(|| format!("running {label}"))?;
    let code = output.status.code().unwrap_or(-1);
    if ok_codes.contains(&code) {
        return Ok(());
    }
    bail!(
        "{label} failed (exit {code}):\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
