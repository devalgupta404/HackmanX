//! Developer tasks that need a QEMU virtual machine. Nothing here ever touches a real disk:
//! every destructive operation happens on image files attached to a throwaway guest.
//!
//! The guest is an Ubuntu cloud image booting with UEFI firmware (OVMF), its disk attached as
//! SATA so SMART works (this QEMU's NVMe rejects the health log page), provisioned once
//! through cloud-init and then reused as `prepared.qcow2`.

mod guest;

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use guest::{Guest, Paths};

/// Whether the automated commands should open the VM's screen (BOOTSHIFT_VM_DISPLAY=1).
fn display_from_env() -> bool {
    std::env::var_os("BOOTSHIFT_VM_DISPLAY").is_some()
}

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Build the provisioned guest image (`xtask/out/prepared.qcow2`): packages, the static
    /// bootshift binary, and a data partition full of checksummed files.
    PrepareVm {
        /// Rebuild even if the prepared image already exists.
        #[arg(long)]
        fresh: bool,
    },
    /// Boot the guest and run the privileged tests inside it.
    VmTest,
    /// The end-to-end proof: apply a plan in the guest, restart into recovery, cut the power
    /// mid-move, and watch the resume finish with every file intact.
    BootProof {
        /// Skip the power cut and let the first recovery run finish undisturbed.
        #[arg(long)]
        no_power_cut: bool,
    },
    /// Boot the guest with its screen in a window and leave it running, set up for a
    /// hand-driven demo: you SSH in and run bootshift yourself. Prints the commands.
    ///
    /// Without `--fresh`, relaunching reuses the same disk, so a run interrupted mid-move
    /// (closing the window is a power cut) resumes from the journal on the next boot.
    Demo {
        /// Start a clean demo, discarding any disk from a previous `demo` run.
        #[arg(long)]
        fresh: bool,
    },
}

fn main() -> Result<()> {
    let paths = Paths::discover()?;
    match Cli::parse().command {
        Task::PrepareVm { fresh } => {
            prepare_vm(&paths, fresh)?;
            println!("prepared: {}", paths.prepared().display());
            Ok(())
        }
        Task::VmTest => vm_test(&paths),
        Task::BootProof { no_power_cut } => boot_proof(&paths, no_power_cut),
        Task::Demo { fresh } => demo(&paths, fresh),
    }
}

/// Boots the guest in a window, installs today's binary, writes a ready target that moves and
/// grows the data partition, then leaves the machine running for you to drive by hand.
fn demo(paths: &Paths, fresh: bool) -> Result<()> {
    let disk = paths.out.join("demo.qcow2");

    // Resume path: reuse the existing disk so an interrupted move can finish. The firmware
    // boot order that `apply` set is in the kept vars file, so the next boot lands in recovery.
    if disk.exists() && !fresh {
        let guest = Guest::launch(paths, &disk, true)?;
        let port = guest.ssh_port();
        let key = paths.ssh_key.display();
        guest.leak();
        println!("\n================  BootShift demo (resuming)  ================\n");
        println!("Reusing the existing disk. If a move was interrupted, watch the VM window:");
        println!("the BootShift recovery console resumes from the journal and finishes, then");
        println!("the machine boots normally.\n");
        println!("Once it reaches the login screen, reconnect and show the result:\n");
        println!("  ssh -p {port} -i {key} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1");
        println!("  sudo bootshift probe /dev/sda\n");
        println!("To start over from a clean disk instead:  cargo xtask demo --fresh");
        println!("Stop the VM when done:  pkill -f demo.qcow2\n");
        println!("============================================================\n");
        return Ok(());
    }

    prepare_vm(paths, false)?;
    let disk = overlay_of(paths, &paths.prepared(), "demo.qcow2")?;
    let mut guest = Guest::launch(paths, &disk, true)?;
    guest.wait_ssh()?;

    build_musl_bootshift(paths)?;
    guest.scp(&paths.musl_binary(), "/tmp/bootshift")?;
    guest.ssh("sudo install -m 0755 /tmp/bootshift /usr/local/sbin/bootshift")?;
    // A console password, so the VM window is usable too, not only SSH.
    guest.ssh("echo 'ubuntu:bootshift' | sudo chpasswd")?;

    let disk_node = guest_disk(&mut guest)?;
    let data_node = partition_of(&disk_node, 2);
    let table = guest.sfdisk_table(&disk_node)?;
    let data = table
        .partitions
        .iter()
        .find(|p| p.node == data_node)
        .context("the prepared image has no data partition")?;
    let partuuid = guest
        .ssh(&format!("sudo blkid -s PARTUUID -o value {data_node}"))?
        .trim()
        .to_string();
    let template = guest.ssh(&format!("sudo bootshift probe {disk_node} --target-template"))?;
    let mut target: serde_json::Value = serde_json::from_str(&template)?;
    let entry = target
        .get_mut(&partuuid)
        .with_context(|| format!("the target template has no {partuuid}"))?;
    entry["start"] = (data.start - GAP_SECTORS).into();
    entry["len"] = (data.size + GAP_SECTORS).into();
    guest.ssh(&format!(
        "cat > /home/ubuntu/move-left.json <<'EOF'\n{}\nEOF",
        serde_json::to_string_pretty(&target)?
    ))?;
    let port = guest.ssh_port();
    let key = paths.ssh_key.display();
    guest.leak();

    println!("\n================  BootShift demo is ready  ================\n");
    println!("The VM window is open on this screen. Keep it visible: the recovery");
    println!("console appears there during the reboot.\n");
    println!("In a NEW terminal, connect to the demo machine:\n");
    println!("  ssh -p {port} -i {key} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1\n");
    println!("Then run these, one at a time:\n");
    println!("  # 1. BEFORE: show the partitions");
    println!("  sudo bootshift probe {disk_node}\n");
    println!("  # 2. See what the change will do (moves the data partition left, then grows it)");
    println!("  sudo env BOOTSHIFT_EXTRA_KERNEL_ARGS='console=tty0 console=ttyS0' \\");
    println!("       bootshift apply {disk_node} /home/ubuntu/move-left.json --yes\n");
    println!("  # 3. Reboot — watch the BootShift recovery console in the VM window");
    println!("  sudo reboot\n");
    println!("  #    (optional: to show crash-safety, close the VM window mid-move to cut the");
    println!("  #     power, then relaunch with: cargo xtask demo — it resumes from the journal)\n");
    println!("  # 4. AFTER: reconnect once it boots back, and show the partitions moved");
    println!("  sudo bootshift probe {disk_node}\n");
    println!("The data partition's start and size will have changed; every file is intact.");
    println!("Stop the VM when done:  pkill -f demo.qcow2\n");
    println!("==========================================================\n");
    Ok(())
}

/// 1 MiB alignment in 512-byte sectors.
const ALIGN: u64 = 2048;
/// How far the data partition moves left, in sectors (768 MiB).
const GAP_SECTORS: u64 = 768 * 2048;
/// The data partition's size, in sectors (2 GiB).
const DATA_SECTORS: u64 = 2 * 1024 * 2048;
/// How many MiB of random files the proof copies and verifies.
const DATA_MIB: u32 = 1200;

fn prepare_vm(paths: &Paths, fresh: bool) -> Result<()> {
    // The qcow2 exists from the moment it is created, long before provisioning finishes, so
    // a marker file says whether it is actually ready.
    let marker = paths.out.join("prepared.ok");
    if marker.exists() && paths.prepared().exists() && !fresh {
        return Ok(());
    }
    let _ = std::fs::remove_file(&marker);
    build_musl_bootshift(paths)?;
    std::fs::create_dir_all(&paths.out)?;
    let prepared = paths.prepared();
    let _ = std::fs::remove_file(&prepared);

    run(Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(&paths.base_image)
        .arg(&prepared)
        .arg("12G"))?;
    guest::write_seed(paths)?;

    println!("booting the guest for provisioning (first boot runs apt)…");
    let mut guest = Guest::launch(paths, &prepared, display_from_env())?;
    guest.wait_ssh()?;
    guest.ssh("cloud-init status --wait >/dev/null; cloud-init status")?;
    for tool in ["zstd", "ntfsresize", "smartctl", "efibootmgr"] {
        guest
            .ssh(&format!("command -v {tool}"))
            .with_context(|| format!("{tool} missing in the guest after provisioning"))?;
    }

    println!("adding the data partition and {DATA_MIB} MiB of checksummed files…");
    let disk_node = guest_disk(&mut guest)?;
    let data_node = partition_of(&disk_node, 2);
    // qemu-img grew the virtual disk, but the GPT still describes the base image's size.
    // Move the backup structures to the new end so the usable area covers the added space.
    guest.ssh(&format!("sudo sgdisk -e {disk_node} && sudo partx --update {disk_node}"))?;
    let table = guest.sfdisk_table(&disk_node)?;
    let root_end = table
        .partitions
        .iter()
        .map(|p| p.start + p.size)
        .max()
        .context("the guest disk has no partitions")?;
    let data_start = (root_end + GAP_SECTORS).next_multiple_of(ALIGN);
    if let Some(last) = table.lastlba {
        if data_start + DATA_SECTORS > last {
            bail!(
                "no room for the data partition: it would end at {} but the disk's usable area ends at {last}",
                data_start + DATA_SECTORS
            );
        }
    }
    // The append is safe on the in-use disk (it only adds an entry), so the in-use and reread
    // checks are overruled; partx tells the kernel afterwards.
    guest.ssh(&format!(
        "echo 'start={data_start}, size={DATA_SECTORS}, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4' \
         | sudo sfdisk --append --force --no-reread {disk_node} && sudo partx --update {disk_node}"
    ))?;
    guest.ssh(&format!(
        "sudo mkfs.ext4 -q -L bootproof {data_node} && sudo mkdir -p /mnt/data \
         && sudo mount {data_node} /mnt/data",
    ))?;
    guest.ssh(&format!(
        "for i in $(seq 1 {}); do sudo dd if=/dev/urandom of=/mnt/data/file$i bs=1M count=100 \
         status=none; done && sudo sh -c 'cd /mnt/data && sha256sum file* > MANIFEST' \
         && sudo umount /mnt/data",
        DATA_MIB / 100
    ))?;

    println!("installing the static bootshift binary…");
    guest.scp(&paths.musl_binary(), "/tmp/bootshift")?;
    guest.ssh("sudo install -m 0755 /tmp/bootshift /usr/local/sbin/bootshift && sudo bootshift --version")?;

    guest.poweroff()?;
    std::fs::write(&marker, b"ok\n")?;
    Ok(())
}

fn vm_test(paths: &Paths) -> Result<()> {
    prepare_vm(paths, false)?;
    let overlay = overlay_of(paths, &paths.prepared(), "vm-test.qcow2")?;
    let mut guest = Guest::launch(paths, &overlay, display_from_env())?;
    guest.wait_ssh()?;

    // The sample privileged test of task 2.5: mkfs on a loop device, inside the guest only.
    println!("running the loop-device test inside the guest…");
    guest.ssh(
        "sudo truncate -s 64M /tmp/loop.img && device=$(sudo losetup --find --show /tmp/loop.img) \
         && sudo env BOOTSHIFT_TEST_VM=1 mkfs.ext4 -q $device && sudo losetup -d $device",
    )?;
    // The same thing is refused on the host: nothing outside a guest may format anything.
    println!("vm-test passed: the guest formatted a loop device; the host never will.");
    guest.poweroff()?;
    Ok(())
}

fn boot_proof(paths: &Paths, no_power_cut: bool) -> Result<()> {
    prepare_vm(paths, false)?;
    let disk = overlay_of(paths, &paths.prepared(), "boot-proof.qcow2")?;
    let mut guest = Guest::launch(paths, &disk, display_from_env())?;
    guest.wait_ssh()?;

    // The prepared image carries whatever binary provisioning installed; the proof always
    // tests today's build.
    build_musl_bootshift(paths)?;
    guest.scp(&paths.musl_binary(), "/tmp/bootshift")?;
    guest.ssh("sudo install -m 0755 /tmp/bootshift /usr/local/sbin/bootshift")?;

    let disk_node = guest_disk(&mut guest)?;
    let data_node = partition_of(&disk_node, 2);
    let table = guest.sfdisk_table(&disk_node)?;
    let data = table
        .partitions
        .iter()
        .find(|p| p.node == data_node)
        .context("the prepared image has no data partition")?;
    let partuuid = guest
        .ssh(&format!("sudo blkid -s PARTUUID -o value {data_node}"))?
        .trim()
        .to_string();
    let new_start = data.start - GAP_SECTORS;
    let new_len = data.size + GAP_SECTORS;
    let boot_order_before = guest.ssh("sudo efibootmgr | grep ^BootOrder")?;

    println!("planning: move the data partition left by {GAP_SECTORS} sectors and grow it…");
    let template = guest.ssh(&format!("sudo bootshift probe {disk_node} --target-template"))?;
    let mut target: serde_json::Value = serde_json::from_str(&template)?;
    let entry = target
        .get_mut(&partuuid)
        .with_context(|| format!("the target template has no {partuuid}"))?;
    entry["start"] = new_start.into();
    entry["len"] = new_len.into();
    guest.ssh(&format!(
        "cat > /tmp/target.json <<'EOF'\n{}\nEOF",
        serde_json::to_string_pretty(&target)?
    ))?;

    println!("applying (this builds the recovery initramfs in the guest)…");
    // The serial console feeds this harness. With a display window, the screen is a console
    // too, so recovery is watchable; its messages go through printk and reach both once.
    let consoles = if std::env::var_os("BOOTSHIFT_VM_DISPLAY").is_some() {
        "console=tty0 console=ttyS0"
    } else {
        "console=ttyS0"
    };
    let applied = guest.ssh(&format!(
        "sudo env BOOTSHIFT_EXTRA_KERNEL_ARGS='{consoles}' \
         bootshift apply {disk_node} /tmp/target.json --yes",
    ))?;
    if !applied.contains("Everything is ready") {
        bail!("apply did not finish:\n{applied}");
    }

    println!("restarting into recovery…");
    let serial_before = guest.serial_len();
    let _ = guest.ssh("sudo reboot");

    guest.wait_serial(serial_before, "BootShift recovery", 180)?;
    println!("recovery is running.");

    if no_power_cut {
        guest.wait_serial(serial_before, "Done. Restarting into your system.", 600)?;
    } else {
        let cut_at = guest.wait_serial_percent(serial_before, 5, 90, 180)?;
        println!("cutting the power at {cut_at}%…");
        guest.kill();
        drop(guest);

        println!("powering back on…");
        guest = Guest::launch(paths, &disk, display_from_env())?;
        guest.wait_serial(0, "BootShift recovery", 180)?;
        guest.wait_serial(0, "Done. Restarting into your system.", 600)?;
        println!("recovery resumed after the power cut and finished.");
    }

    guest.wait_ssh()?;
    println!("the system booted normally; verifying…");

    let disk_node = guest_disk(&mut guest)?;
    let data_node = partition_of(&disk_node, 2);
    let check = guest.ssh(&format!(
        "sudo mkdir -p /mnt/data && sudo mount {data_node} /mnt/data \
         && cd /mnt/data && sudo sha256sum --quiet -c MANIFEST && echo FILES-OK \
         && cd / && sudo umount /mnt/data",
    ))?;
    if !check.contains("FILES-OK") {
        bail!("file verification failed:\n{check}");
    }
    let table = guest.sfdisk_table(&disk_node)?;
    let moved = table
        .partitions
        .iter()
        .find(|p| p.node == data_node)
        .context("the data partition vanished")?;
    if moved.start != new_start || moved.size != new_len {
        bail!(
            "the table does not hold the target: start {} len {}, wanted {new_start} and {new_len}",
            moved.start,
            moved.size
        );
    }
    let grub = guest.ssh("sudo cat /boot/efi/EFI/ubuntu/grub.cfg")?;
    if grub.contains("bootshift") {
        bail!("the GRUB redirect was not removed:\n{grub}");
    }
    let boot_order_after = guest.ssh("sudo efibootmgr | grep ^BootOrder")?;
    if boot_order_after.trim() != boot_order_before.trim() {
        bail!("BootOrder was not restored: {boot_order_before} vs {boot_order_after}");
    }
    // vfat mounts root-only (dmask=0077), so the listing needs sudo.
    let journal = guest.ssh(
        "sudo mount /boot/efi 2>/dev/null; sudo ls /boot/efi/EFI/bootshift/ && echo ESP-OK",
    )?;
    if !journal.contains("ESP-OK") {
        bail!("the ESP looks wrong:\n{journal}");
    }

    guest.poweroff()?;
    println!();
    println!("BOOT PROOF PASSED");
    println!("  moved and grew the data partition on the guest's own disk");
    if !no_power_cut {
        println!("  survived a power cut mid-move and resumed from the journal");
    }
    println!("  every file byte-identical, table on target, booting restored");
    Ok(())
}

/// The guest's one disk, whatever the controller calls it.
fn guest_disk(guest: &mut Guest) -> Result<String> {
    let path = guest.ssh("lsblk --nodeps --noheadings --raw --output PATH,TYPE | awk '$2==\"disk\"{print $1; exit}'")?;
    let path = path.trim().to_string();
    if path.is_empty() {
        bail!("the guest reports no disk");
    }
    Ok(path)
}

/// `/dev/sda` + 2 → `/dev/sda2`; `/dev/nvme0n1` + 2 → `/dev/nvme0n1p2`.
fn partition_of(disk: &str, number: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{number}")
    } else {
        format!("{disk}{number}")
    }
}

fn build_musl_bootshift(paths: &Paths) -> Result<()> {
    println!("building the static bootshift binary…");
    run(Command::new("cargo")
        .current_dir(&paths.repo)
        .args([
            "build",
            "-p",
            "bootshift-cli",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
        ]))
}

fn overlay_of(paths: &Paths, base: &PathBuf, name: &str) -> Result<PathBuf> {
    let overlay = paths.out.join(name);
    let _ = std::fs::remove_file(&overlay);
    run(Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(base)
        .arg(&overlay))?;
    Ok(overlay)
}

fn run(command: &mut Command) -> Result<()> {
    let printable = format!("{command:?}");
    let status = command.status().with_context(|| printable.clone())?;
    if !status.success() {
        bail!("{printable} failed with {status}");
    }
    Ok(())
}
