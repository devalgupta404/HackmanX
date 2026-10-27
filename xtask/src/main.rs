//! Developer tasks that need a QEMU virtual machine. Nothing here ever touches a real disk:
//! every destructive operation happens on image files attached to a throwaway guest.
//!
//! The guest is an Ubuntu cloud image booting with UEFI firmware (OVMF), its disk attached as
//! SATA so SMART works (this QEMU's NVMe rejects the health log page), provisioned once
//! through cloud-init and then reused as `prepared.qcow2`.

mod guest;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use guest::{ExtraDisk, Guest, LaunchOpts, Paths};

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
    /// Boot the guest, run bootshiftd inside it, and forward its system D-Bus to this machine,
    /// so the desktop GUI on this box can drive the guest's real disk. Prints the command to
    /// launch the GUI. The guest's screen (in a window) shows the recovery console on reboot.
    DemoGui {
        /// Start a clean disk, discarding any from a previous `demo-gui` run.
        #[arg(long)]
        fresh: bool,
    },
    /// Machine-to-machine proof: boot two guests, fill a data disk on the source, migrate it to a
    /// blank disk on the target over a real TCP link, and verify the target is byte-identical and
    /// its filesystem mounts with every file intact.
    MigrateRig {
        /// Size of the data disk to migrate, in MiB.
        #[arg(long, default_value_t = 256)]
        data_mib: u64,
        /// Live migration: keep the source filesystem mounted and churning while it migrates, with
        /// the eBPF tracker, a freeze cutover and a signed receipt.
        #[arg(long)]
        live: bool,
        /// Reshape: the source is a partitioned disk migrated onto a larger target, then finalize
        /// grows the partition and filesystem to fill it.
        #[arg(long)]
        reshape: bool,
        /// Mirror mode: the source mirrors its LVM volume onto the target over nvme-tcp, then the
        /// target promotes the synced leg.
        #[arg(long)]
        mirror: bool,
        /// Rehearsal: after the copy, take a writable dm-snapshot of the replica and prove it is a
        /// testable copy whose writes never touch the replica.
        #[arg(long)]
        rehearse: bool,
    },
    /// Hand-driven migration demo: boot two guests (this machine and a blank new one) in windows,
    /// keep the source live and churning, open a terminal into each, and launch the BootShift GUI
    /// on its migration screen. You check the disks in each terminal, press Start migration in the
    /// GUI, and check again: the new machine ends byte-identical with a signed receipt.
    MigrateDemo {
        /// Size of the data disk to migrate, in MiB.
        #[arg(long, default_value_t = 256)]
        data_mib: u64,
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
        Task::DemoGui { fresh } => demo_gui(&paths, fresh),
        Task::MigrateRig {
            data_mib,
            live,
            reshape,
            mirror,
            rehearse,
        } => migrate_rig(&paths, data_mib, live, reshape, mirror, rehearse),
        Task::MigrateDemo { data_mib } => migrate_demo(&paths, data_mib),
    }
}

/// The host-side D-Bus socket the GUI connects to; ssh forwards it to the guest's system bus.
const FORWARDED_BUS: &str = "/tmp/bootshift-vm-bus";

/// Boots the guest, installs and starts bootshiftd in it, forwards its system bus to this box,
/// and prints the command that launches the desktop GUI against the guest's real disk.
fn demo_gui(paths: &Paths, fresh: bool) -> Result<()> {
    let disk = paths.out.join("demo-gui.qcow2");
    let reused = !fresh && disk.exists();
    if !reused {
        prepare_vm(paths, false)?;
        reset_demo_gui_disk(paths, &disk)?;
    }
    // Show the VM window when this box has a desktop; headless when run over SSH.
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let mut guest = Guest::launch(paths, &disk, has_display)?;
    if let Err(first_try) = guest.wait_ssh() {
        if !reused {
            return Err(first_try);
        }
        // A reused overlay that never answers almost always means a dirty shutdown corrupted its
        // ext4 root. The cloud image's initramfs has no fsck, so it can't self-repair and drops
        // to emergency mode before the network is up. The overlay is disposable, so rebuild it
        // from the pristine prepared image and boot once more.
        eprintln!(
            "the reused demo disk never came up; a dirty shutdown likely corrupted its root \
             filesystem. Rebuilding it fresh from the prepared image…"
        );
        guest.kill();
        reset_demo_gui_disk(paths, &disk)?;
        guest = Guest::launch(paths, &disk, has_display)?;
        guest.wait_ssh()?;
    }

    println!("installing bootshiftd in the guest…");
    build_musl_bootshift(paths)?;
    let helper = build_helper(paths)?;
    guest.ssh("sudo mkdir -p /usr/libexec/bootshift")?;
    guest.scp(&paths.musl_binary(), "/tmp/bootshift")?;
    guest.scp(&helper, "/tmp/bootshiftd")?;
    guest.ssh(
        "sudo install -m0755 /tmp/bootshift /usr/libexec/bootshift/bootshift \
         && sudo install -m0755 /tmp/bootshiftd /usr/libexec/bootshift/bootshiftd",
    )?;
    // The D-Bus policy lets root own the name; the GUI reaches it over the forwarded bus.
    guest.scp(
        &paths.repo.join("packaging/dbus/org.bootshift.Helper1.conf"),
        "/tmp/bootshift.conf",
    )?;
    guest.ssh(
        "sudo install -m0644 /tmp/bootshift.conf /usr/share/dbus-1/system.d/org.bootshift.Helper1.conf \
         && sudo systemctl reload dbus 2>/dev/null; true",
    )?;

    println!("starting bootshiftd (authorization waived for the demo)…");
    // Run it as a transient root unit with the demo polkit bypass; stop any prior one by name
    // (never pkill -f: its pattern would match the shell running it and kill this SSH session).
    guest.ssh("sudo systemctl reset-failed bootshiftd-demo 2>/dev/null; sudo systemctl stop bootshiftd-demo 2>/dev/null; true")?;
    guest.ssh(
        "sudo systemd-run --unit=bootshiftd-demo --setenv=BOOTSHIFT_SKIP_POLKIT=1 \
         /usr/libexec/bootshift/bootshiftd",
    )?;
    let version = guest
        .ssh("for i in $(seq 1 10); do out=$(dbus-send --system --print-reply --dest=org.bootshift.Helper1 /org/bootshift/Helper1 org.bootshift.Helper1.Version 2>&1); echo \"$out\" | grep -q string && { echo \"$out\" | tail -1; exit 0; }; sleep 1; done; echo TIMEOUT")
        .unwrap_or_default();
    if !version.contains("string") {
        bail!("bootshiftd did not answer on the guest's system bus:\n{version}");
    }
    println!("bootshiftd answers on the guest's bus: {}", version.trim());

    let port = guest.ssh_port();
    let key = paths.ssh_key.clone();
    guest.leak();

    // Forward the guest's system bus socket to this box as a unix socket the GUI can use.
    let _ = std::fs::remove_file(FORWARDED_BUS);
    Command::new("ssh")
        .args(["-p", &port.to_string()])
        .arg("-i")
        .arg(&key)
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StreamLocalBindUnlink=yes",
            "-fN",
            "-L",
        ])
        .arg(format!("{FORWARDED_BUS}:/run/dbus/system_bus_socket"))
        .arg("ubuntu@127.0.0.1")
        .status()
        .context("starting the D-Bus forward")?;

    let gui = paths.repo.join("target/release/bootshift-gui");
    println!("\n================  BootShift GUI demo is ready  ================\n");
    println!("The guest's bus is forwarded to {FORWARDED_BUS}. Build the GUI once:\n");
    println!("  cargo build -p bootshift-gui --release\n");
    println!("Then launch the desktop app against the guest's REAL disk:\n");
    println!(
        "  DBUS_SYSTEM_BUS_ADDRESS=unix:path={FORWARDED_BUS} {}\n",
        gui.display()
    );
    println!("Drag a partition, run the checks, and apply. When it says to restart, reboot the");
    println!("guest to watch the recovery console in the VM window:\n");
    println!(
        "  ssh -p {port} -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1 sudo reboot\n",
        key.display()
    );
    println!("After it boots back, reopen the GUI to see the new layout.");
    println!("Stop everything:  pkill -f demo-gui.qcow2; pkill -f {FORWARDED_BUS}\n");
    println!("==============================================================\n");
    Ok(())
}

/// Recreates the demo-gui overlay on top of the pristine prepared image, discarding any disk and
/// firmware vars from a previous run so the boot starts clean.
fn reset_demo_gui_disk(paths: &Paths, disk: &Path) -> Result<()> {
    let _ = std::fs::remove_file(disk);
    let _ = std::fs::remove_file(disk.with_extension("vars.fd"));
    run(Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(paths.prepared())
        .arg(disk))?;
    Ok(())
}

fn build_helper(paths: &Paths) -> Result<PathBuf> {
    println!("building bootshiftd…");
    run(Command::new("cargo").current_dir(&paths.repo).args([
        "build",
        "-p",
        "bootshift-helper",
        "--release",
    ]))?;
    Ok(paths.repo.join("target/release/bootshiftd"))
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
        println!(
            "  ssh -p {port} -i {key} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1"
        );
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
    let template = guest.ssh(&format!(
        "sudo bootshift probe {disk_node} --target-template"
    ))?;
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
    println!(
        "  ssh -p {port} -i {key} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1\n"
    );
    println!("Then run these, one at a time:\n");
    println!("  # 1. BEFORE: show the partitions");
    println!("  sudo bootshift probe {disk_node}\n");
    println!("  # 2. See what the change will do (moves the data partition left, then grows it)");
    println!("  sudo env BOOTSHIFT_EXTRA_KERNEL_ARGS='console=tty0 console=ttyS0' \\");
    println!("       bootshift apply {disk_node} /home/ubuntu/move-left.json --yes\n");
    println!("  # 3. Reboot — watch the BootShift recovery console in the VM window");
    println!("  sudo reboot\n");
    println!("  #    (optional: to show crash-safety, close the VM window mid-move to cut the");
    println!(
        "  #     power, then relaunch with: cargo xtask demo — it resumes from the journal)\n"
    );
    println!("  # 4. AFTER: reconnect once it boots back, and show the partitions moved");
    println!("  sudo bootshift probe {disk_node}\n");
    println!("The data partition's start and size will have changed; every file is intact.");
    println!("Stop the VM when done:  pkill -f demo.qcow2\n");
    println!("==========================================================\n");
    Ok(())
}

/// 1 MiB alignment in 512-byte sectors.
const ALIGN: u64 = 2048;
/// How far the data partition moves left, in sectors (512 MiB). This is also how much the move
/// grows it by, so the free gap in front becomes slack the demo can grow into and shrink back.
const GAP_SECTORS: u64 = 512 * 2048;
/// The data partition's size, in sectors (7 GiB). Deliberately far larger than the data it holds
/// so a Btrfs shrink has room to give back and a later grow has room to take, both on screen.
const DATA_SECTORS: u64 = 7 * 1024 * 2048;
/// How many MiB of random files fill the data partition. The move copies Btrfs's allocated extents
/// (one data chunk plus a little metadata), not the whole partition, so this stays a small fraction
/// of DATA_SECTORS and the partition keeps real slack. Sized so the copy runs for a watchable spell
/// at this box's KVM speed, where the crash-safe copier's per-chunk flush holds it near 30 MiB/s.
const DATA_MIB: u32 = 400;

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
    for tool in ["zstd", "ntfsresize", "smartctl", "efibootmgr", "mkfs.btrfs"] {
        guest
            .ssh(&format!("command -v {tool}"))
            .with_context(|| format!("{tool} missing in the guest after provisioning"))?;
    }

    println!("adding the data partition and {DATA_MIB} MiB of checksummed files…");
    let disk_node = guest_disk(&mut guest)?;
    let data_node = partition_of(&disk_node, 2);
    // qemu-img grew the virtual disk, but the GPT still describes the base image's size.
    // Move the backup structures to the new end so the usable area covers the added space.
    guest.ssh(&format!(
        "sudo sgdisk -e {disk_node} && sudo partx --update {disk_node}"
    ))?;
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
    // Btrfs, not ext4: it is the one filesystem this build can move, grow AND shrink today,
    // so the demo shows all three. Its online resize is journaled, so it needs no guarded run.
    // Single (not DUP) metadata keeps the allocated extents the copier moves down to roughly the
    // data itself, so the move stays a watchable span rather than minutes on this slow qcow2.
    guest.ssh(&format!(
        "sudo mkfs.btrfs -q -f -m single -L bootproof {data_node} && sudo mkdir -p /mnt/data \
         && sudo mount {data_node} /mnt/data",
    ))?;
    guest.ssh(&format!(
        "for i in $(seq 1 {}); do sudo dd if=/dev/urandom of=/mnt/data/file$i bs=1M count=100 \
         status=none; done && sudo sh -c 'cd /mnt/data && sha256sum file* > MANIFEST' \
         && sudo sync /mnt/data && sudo umount /mnt/data",
        DATA_MIB / 100
    ))?;

    println!("installing the static bootshift binary…");
    guest.scp(&paths.musl_binary(), "/tmp/bootshift")?;
    guest.ssh(
        "sudo install -m 0755 /tmp/bootshift /usr/local/sbin/bootshift && sudo bootshift --version",
    )?;

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
    let template = guest.ssh(&format!(
        "sudo bootshift probe {disk_node} --target-template"
    ))?;
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
    let journal = guest
        .ssh("sudo mount /boot/efi 2>/dev/null; sudo ls /boot/efi/EFI/bootshift/ && echo ESP-OK")?;
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

/// Boots two guests and migrates a filled data disk on the source to a blank disk on the target
/// over a real TCP link, then proves the target is byte-identical and its filesystem mounts intact.
/// The link runs through the host loopback: the target forwards a host port to its receiver, and the
/// source reaches it as `10.0.2.2:<port>` (its view of the host). Nothing touches a real disk.
fn migrate_rig(
    paths: &Paths,
    data_mib: u64,
    live: bool,
    reshape: bool,
    mirror: bool,
    rehearse: bool,
) -> Result<()> {
    prepare_vm(paths, false)?;
    if mirror {
        return migrate_mirror_rig(paths, data_mib);
    }
    if rehearse {
        return migrate_rehearse_rig(paths, data_mib);
    }
    // Live mode needs the eBPF tracker, which libbpf ties to glibc, so it uses the dynamic binary;
    // the plain copy and reshape use the static musl binary.
    let migrate_bin = if live {
        build_migrate_ebpf(paths)?
    } else {
        build_musl_migrate(paths)?
    };

    let data_bytes = data_mib * (1 << 20);
    let scratch_bytes = 64u64 * (1 << 20);
    // Reshape migrates the source onto a larger target disk and grows into it.
    let target_bytes = if reshape { data_bytes * 3 } else { data_bytes };

    let src_root = overlay_of(paths, &paths.prepared(), "mig-src.qcow2")?;
    let tgt_root = overlay_of(paths, &paths.prepared(), "mig-tgt.qcow2")?;
    let src_data = paths.out.join("mig-src-data.raw");
    let tgt_data = paths.out.join("mig-tgt-data.raw");
    let tgt_scratch = paths.out.join("mig-tgt-scratch.raw");
    create_raw(&src_data, data_bytes)?;
    create_raw(&tgt_data, target_bytes)?;
    create_raw(&tgt_scratch, scratch_bytes)?;

    let display = display_from_env();
    // A host port the source reaches the target's receiver through (host loopback relays it).
    let mig_port = guest::free_port()?;

    println!("booting the target guest (the receiver)…");
    let mut target = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &tgt_root,
            display,
            extra_disks: &[
                ExtraDisk {
                    image: tgt_data.clone(),
                    serial: "tgtdata".into(),
                },
                ExtraDisk {
                    image: tgt_scratch.clone(),
                    serial: "tgtscr".into(),
                },
            ],
            extra_hostfwd: &[(mig_port, 9876)],
        },
    )?;
    target.wait_ssh()?;

    println!("booting the source guest (the sender)…");
    let mut source = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &src_root,
            display,
            extra_disks: &[ExtraDisk {
                image: src_data.clone(),
                serial: "srcdata".into(),
            }],
            extra_hostfwd: &[],
        },
    )?;
    source.wait_ssh()?;

    let src_dev = "/dev/disk/by-id/virtio-srcdata";
    let tgt_dev = "/dev/disk/by-id/virtio-tgtdata";
    let scr_dev = "/dev/disk/by-id/virtio-tgtscr";

    println!("filling the source data disk with a filesystem and checksummed files…");
    let files_cmd = format!(
        "for i in $(seq 1 {}); do sudo dd if=/dev/urandom of=/mnt/d/file$i bs=1M count=8 status=none; done \
         && sudo sh -c 'cd /mnt/d && sha256sum file* > MANIFEST' && sudo sync",
        (data_mib / 16).max(1),
    );
    if reshape {
        // A partitioned disk: GPT + one ext4 partition, so finalize has a partition to grow.
        source.ssh(&format!("sudo sgdisk -n 1:0:0 -t 1:8300 {src_dev} >/dev/null && sudo partprobe {src_dev} 2>/dev/null; sudo partx -u {src_dev} 2>/dev/null; true"))?;
        let src_part = format!("{src_dev}-part1");
        source.ssh(&format!(
            "sudo mkfs.ext4 -q -F {src_part} && sudo mkdir -p /mnt/d && sudo mount {src_part} /mnt/d \
             && {files_cmd} && sudo umount /mnt/d"
        ))?;
    } else {
        source.ssh(&format!("sudo mkfs.ext4 -q -F {src_dev}"))?;
        if live {
            // Keep the source mounted and churning: the tracker captures the writes, and the freeze
            // cutover plus the receipt prove the target equals the frozen image.
            source.ssh(&format!(
                "sudo mkdir -p /mnt/d && sudo mount {src_dev} /mnt/d && {files_cmd}"
            ))?;
        } else {
            source.ssh(&format!(
                "sudo mkdir -p /mnt/d && sudo mount {src_dev} /mnt/d && {files_cmd} && sudo umount /mnt/d"
            ))?;
        }
    }

    println!("installing the migrate binary in both guests…");
    source.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    source.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;
    target.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    target.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;

    println!("starting the receiver on the target…");
    target.ssh(&format!(
        "sudo systemctl reset-failed migrate-recv 2>/dev/null; true; \
         sudo systemd-run --unit=migrate-recv --collect /bin/sh -c \
         'exec /usr/local/sbin/bootshift-migrate receive --data {tgt_dev} --scratch {scr_dev} \
          --listen 0.0.0.0:9876 --receipt /tmp/receipt.json > /tmp/recv.log 2>&1'"
    ))?;
    // Wait until the receiver is actually listening before the source dials it.
    target.ssh("for i in $(seq 1 30); do ss -ltn 2>/dev/null | grep -q ':9876' && exit 0; sleep 0.5; done; echo 'receiver never listened'; cat /tmp/recv.log 2>/dev/null; exit 1")
        .context("the receiver never started listening")?;

    if reshape {
        migrate_reshape(
            &mut source,
            &mut target,
            src_dev,
            tgt_dev,
            mig_port,
            data_mib,
        )?;
    } else if live {
        migrate_live(&mut source, &mut target, src_dev, tgt_dev, mig_port)?;
    } else {
        migrate_copy(&mut source, &mut target, src_dev, tgt_dev, mig_port)?;
    }

    source.poweroff()?;
    target.poweroff()?;
    println!();
    println!("MIGRATE RIG PASSED");
    println!("  filled a {data_mib} MiB data disk on the source machine");
    if reshape {
        println!(
            "  migrated it onto a {}x larger disk on a second machine",
            3
        );
        println!("  finalize grew the partition and ext4 filesystem to fill the new disk");
        println!("  the grown filesystem is clean and every file still checks out");
    } else if live {
        println!("  migrated it LIVE (mounted and churning) to a second machine over a real link");
        println!("  froze the source, drained the last writes, and signed a byte-equality receipt");
        println!("  the migrated filesystem is clean and mounts on the target");
    } else {
        println!("  migrated it to a blank disk on a second machine over a real TCP link");
        println!("  the target is byte-for-byte identical, and its filesystem mounts intact");
    }
    Ok(())
}

/// Hand-driven migration demo: set the stage, then hand the controls to a person. Boots the two
/// guests in windows, fills the source and keeps it live and churning, installs the migrate binary,
/// starts the receiver, opens a terminal into each guest, and launches the BootShift GUI on its
/// migration screen. The GUI's "Start migration" writes a go-file a background runner waits for; the
/// runner performs the real live migration and drops the receiver's signed receipt where the GUI
/// picks it up. Nothing touches a real disk: both machines are QEMU guests on virtio data disks.
fn migrate_demo(paths: &Paths, data_mib: u64) -> Result<()> {
    prepare_vm(paths, false)?;
    let migrate_bin = build_migrate_ebpf(paths)?;
    println!("building the desktop GUI…");
    run(Command::new("cargo").current_dir(&paths.repo).args([
        "build",
        "-p",
        "bootshift-gui",
        "--release",
    ]))?;
    let gui = paths.repo.join("target/release/bootshift-gui");

    let data_bytes = data_mib * (1 << 20);
    let scratch_bytes = 64u64 * (1 << 20);
    let src_root = overlay_of(paths, &paths.prepared(), "mig-demo-src.qcow2")?;
    let tgt_root = overlay_of(paths, &paths.prepared(), "mig-demo-tgt.qcow2")?;
    let src_data = paths.out.join("mig-demo-src-data.raw");
    let tgt_data = paths.out.join("mig-demo-tgt-data.raw");
    let tgt_scratch = paths.out.join("mig-demo-tgt-scratch.raw");
    create_raw(&src_data, data_bytes)?;
    create_raw(&tgt_data, data_bytes)?;
    create_raw(&tgt_scratch, scratch_bytes)?;

    let mig_port = guest::free_port()?;

    println!("booting the new machine (the target)…");
    let mut target = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &tgt_root,
            display: true,
            extra_disks: &[
                ExtraDisk {
                    image: tgt_data.clone(),
                    serial: "tgtdata".into(),
                },
                ExtraDisk {
                    image: tgt_scratch.clone(),
                    serial: "tgtscr".into(),
                },
            ],
            extra_hostfwd: &[(mig_port, 9876)],
        },
    )?;
    target.wait_ssh()?;

    println!("booting this machine (the source)…");
    let mut source = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &src_root,
            display: true,
            extra_disks: &[ExtraDisk {
                image: src_data.clone(),
                serial: "srcdata".into(),
            }],
            extra_hostfwd: &[],
        },
    )?;
    source.wait_ssh()?;

    let src_dev = "/dev/disk/by-id/virtio-srcdata";
    let tgt_dev = "/dev/disk/by-id/virtio-tgtdata";
    let scr_dev = "/dev/disk/by-id/virtio-tgtscr";

    println!("filling the source with checksummed files and keeping it mounted…");
    let files_cmd = format!(
        "for i in $(seq 1 {}); do sudo dd if=/dev/urandom of=/mnt/d/file$i bs=1M count=8 status=none; done \
         && sudo sh -c 'cd /mnt/d && sha256sum file* > MANIFEST' && sudo sync",
        (data_mib / 16).max(1),
    );
    source.ssh(&format!("sudo mkfs.ext4 -q -F {src_dev}"))?;
    source.ssh(&format!(
        "sudo mkdir -p /mnt/d && sudo mount {src_dev} /mnt/d && {files_cmd}"
    ))?;

    println!("installing the migrate binary in both machines…");
    source.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    source.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;
    target.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    target.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;

    println!("starting the receiver on the new machine…");
    target.ssh(&format!(
        "sudo systemctl reset-failed migrate-recv 2>/dev/null; true; \
         sudo systemd-run --unit=migrate-recv --collect /bin/sh -c \
         'exec /usr/local/sbin/bootshift-migrate receive --data {tgt_dev} --scratch {scr_dev} \
          --listen 0.0.0.0:9876 --receipt /tmp/receipt.json > /tmp/recv.log 2>&1'"
    ))?;
    target.ssh("for i in $(seq 1 30); do ss -ltn 2>/dev/null | grep -q ':9876' && exit 0; sleep 0.5; done; echo 'receiver never listened'; exit 1")
        .context("the receiver never started listening")?;

    // Keep the source changing so the live migration has real churn to chase while the person reads
    // the disks. The send stops this unit when it freezes for cutover.
    source.ssh(
        "sudo systemctl reset-failed migrate-churn 2>/dev/null; true; \
         sudo systemd-run --unit=migrate-churn --collect /bin/sh -c \
         'while true; do dd if=/dev/urandom of=/mnt/d/churn$((RANDOM%6)) bs=256k count=1 \
          conv=notrunc status=none; sync /mnt/d; sleep 0.4; done'",
    )?;

    let src_port = source.ssh_port();
    let tgt_port = target.ssh_port();
    let key = paths.ssh_key.clone();
    let ssh_opts = "-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR";

    // File handshake with the GUI: it writes `go` on Start migration; the runner performs the real
    // migration and drops the receiver's signed receipt at `receipt` for the GUI to display.
    let go = paths.out.join("migrate-demo.go");
    let receipt = paths.out.join("migrate-demo-receipt.json");
    let _ = std::fs::remove_file(&go);
    let _ = std::fs::remove_file(&receipt);

    let runner = paths.out.join("migrate-demo-run.sh");
    std::fs::write(
        &runner,
        format!(
            "#!/bin/bash\n\
         set -u\n\
         while [ ! -e '{go}' ]; do sleep 0.3; done\n\
         rm -f '{go}'\n\
         echo 'running the live migration…'\n\
         ssh -p {src_port} -i {key} {ssh_opts} ubuntu@127.0.0.1 \
           \"sudo /usr/local/sbin/bootshift-migrate send --source {src_dev} --track {src_dev} \
             --to 10.0.2.2:{mig_port} --freeze /mnt/d --budget-ms 2000; \
             sudo systemctl stop migrate-churn 2>/dev/null; true\"\n\
         ssh -p {tgt_port} -i {key} {ssh_opts} ubuntu@127.0.0.1 \
           'for i in $(seq 1 30); do systemctl is-active migrate-recv >/dev/null 2>&1 || break; sleep 1; done'\n\
         scp -P {tgt_port} -i {key} {ssh_opts} ubuntu@127.0.0.1:/tmp/receipt.json '{receipt}.part'\n\
         mv '{receipt}.part' '{receipt}'\n\
         echo 'migration done; receipt at {receipt}'\n",
            go = go.display(),
            key = key.display(),
            receipt = receipt.display(),
        ),
    )?;
    Command::new("bash")
        .arg(&runner)
        .spawn()
        .context("starting the migration runner")?;

    // A terminal into each machine, for the person to check the disks by hand.
    open_guest_terminal("SOURCE — this machine", src_port, &key)?;
    open_guest_terminal("TARGET — the new machine", tgt_port, &key)?;

    // The GUI on its migration screen. It talks to no helper (the screen is self-contained), so the
    // demo backend is fine; the migration env points it at the handshake files.
    Command::new(&gui)
        .arg("--demo")
        .env("BOOTSHIFT_MIGRATE_DEMO", "1")
        .env("BOOTSHIFT_MIGRATE_GO", &go)
        .env("BOOTSHIFT_MIGRATE_RECEIPT", &receipt)
        .spawn()
        .context("launching the GUI")?;

    source.leak();
    target.leak();

    println!("\n================  Migration demo is ready  ================\n");
    println!(
        "Two machines are up in their own windows, plus a terminal into each and the BootShift"
    );
    println!("window on its migration screen. The source is live and changing the whole time.\n");
    println!("1) BEFORE — in the SOURCE terminal, show what is being migrated:");
    println!("     sudo cat /mnt/d/MANIFEST");
    println!("   and in the TARGET terminal, show the new disk is blank:");
    println!("     sudo sha256sum {tgt_dev}\n");
    println!("2) In the BootShift window, press \"Start migration\" and watch it commit.\n");
    println!("3) AFTER — in the TARGET terminal, prove the new machine is identical:");
    println!(
        "     sudo /usr/local/sbin/bootshift-migrate verify-receipt /tmp/receipt.json --target {tgt_dev}"
    );
    println!(
        "     sudo mkdir -p /mnt/p && sudo mount {tgt_dev} /mnt/p && (cd /mnt/p && sudo sha256sum -c MANIFEST)\n"
    );
    println!("Stop everything when done:");
    println!("     pkill -f mig-demo; pkill -f bootshift-gui; pkill -f migrate-demo-run");
    println!("==========================================================\n");
    Ok(())
}

/// Opens a titled terminal with an interactive SSH shell into a guest, for a hand-driven demo.
fn open_guest_terminal(title: &str, port: u16, key: &Path) -> Result<()> {
    Command::new("gnome-terminal")
        .arg(format!("--title={title}"))
        .arg("--")
        .args(["ssh", "-p", &port.to_string(), "-i"])
        .arg(key)
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "ubuntu@127.0.0.1",
        ])
        .spawn()
        .with_context(|| format!("opening the {title} terminal"))?;
    Ok(())
}

/// The plain copy: send the unmounted source, verify the target is byte-identical and its files check.
fn migrate_copy(
    source: &mut Guest,
    target: &mut Guest,
    src_dev: &str,
    tgt_dev: &str,
    mig_port: u16,
) -> Result<()> {
    let src_hash = device_sha(source, src_dev)?;
    println!("sending from the source to the target over the link…");
    let sent = source.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate send --source {src_dev} --to 10.0.2.2:{mig_port}"
    ))?;
    if !sent.contains("copied and verified") {
        bail!("the sender did not report success:\n{sent}");
    }
    println!("sender: {}", sent.trim());

    let recv_log = target.ssh(
        "for i in $(seq 1 30); do systemctl is-active migrate-recv >/dev/null 2>&1 || break; sleep 1; done; \
         cat /tmp/recv.log 2>/dev/null | tail -5",
    )?;
    if !recv_log.contains("written and verified") {
        bail!("the receiver did not report a verified target:\n{recv_log}");
    }
    let tgt_hash = device_sha(target, tgt_dev)?;
    if tgt_hash != src_hash {
        bail!("the target disk differs: source {src_hash} vs target {tgt_hash}");
    }
    let files = target.ssh(&format!(
        "sudo mkdir -p /mnt/d && sudo mount {tgt_dev} /mnt/d \
         && cd /mnt/d && sudo sha256sum --quiet -c MANIFEST && echo FILES-OK; cd /; sudo umount /mnt/d",
    ))?;
    if !files.contains("FILES-OK") {
        bail!("the migrated filesystem failed verification:\n{files}");
    }
    Ok(())
}

/// The live migration: churn the mounted source while it migrates, then check the sender committed
/// with a matching receipt and the migrated filesystem is clean.
fn migrate_live(
    source: &mut Guest,
    target: &mut Guest,
    src_dev: &str,
    tgt_dev: &str,
    mig_port: u16,
) -> Result<()> {
    // BEFORE: show the proof on each machine's OWN screen (its console, /dev/tty1, is what the VM
    // window displays) — the source holds real files, the target disk is blank.
    println!("BEFORE: proving the starting state on each machine's screen…");
    let _ = source.ssh(
        "{ printf '\\n========  SOURCE MACHINE  ========\\n'; echo 'files to migrate, with checksums:'; \
          sudo cat /mnt/d/MANIFEST; printf '=================================\\n\\n'; } | sudo tee /dev/tty1 >/dev/null",
    )?;
    let _ = target.ssh(&format!(
        "{{ printf '\\n========  TARGET MACHINE  ========\\n'; echo 'disk is blank (sha256 of the empty device):'; \
           sudo sha256sum {tgt_dev}; printf '==================================\\n\\n'; }} | sudo tee /dev/tty1 >/dev/null",
    ))?;

    // A background writer keeps changing the mounted source during the migration.
    source.ssh(
        "sudo systemctl reset-failed migrate-churn 2>/dev/null; true; \
         sudo systemd-run --unit=migrate-churn --collect /bin/sh -c \
         'for r in $(seq 1 400); do dd if=/dev/urandom of=/mnt/d/churn$((r%6)) bs=256k count=1 \
          conv=notrunc status=none; sync /mnt/d; sleep 0.2; done'",
    )?;

    println!("migrating live while the source churns…");
    let sent = source.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate send --source {src_dev} --track {src_dev} \
         --to 10.0.2.2:{mig_port} --freeze /mnt/d --budget-ms 2000"
    ))?;
    let _ = source.ssh("sudo systemctl stop migrate-churn 2>/dev/null; true");
    if !sent.contains("migration committed") || !sent.contains("roots match") {
        bail!("the live migration did not commit with a matching receipt:\n{sent}");
    }
    println!("sender:\n{}", sent.trim());

    let recv_log = target.ssh(
        "for i in $(seq 1 30); do systemctl is-active migrate-recv >/dev/null 2>&1 || break; sleep 1; done; \
         cat /tmp/recv.log 2>/dev/null | tail -6",
    )?;
    if !recv_log.contains("written and verified") {
        bail!("the receiver did not report a verified target:\n{recv_log}");
    }
    println!("receiver:\n{}", recv_log.trim());

    // AFTER: show the proof on the TARGET machine's own screen — an independent verify-receipt
    // against its disk, and the file-by-file checksum check. tee both to its console and to us.
    println!("AFTER: proving the result on the target machine's screen…");
    let proof = target.ssh(&format!(
        "{{ printf '\\n======  TARGET MACHINE: PROOF  ======\\n'; \
           echo '>> verify the signed receipt against THIS disk:'; \
           sudo /usr/local/sbin/bootshift-migrate verify-receipt /tmp/receipt.json --target {tgt_dev}; \
           echo; echo '>> every file now matches the source manifest:'; \
           sudo mkdir -p /mnt/p && sudo mount {tgt_dev} /mnt/p && (cd /mnt/p && sudo sha256sum -c MANIFEST 2>/dev/null); \
           sudo umount /mnt/p 2>/dev/null || true; printf '=====================================\\n\\n'; }} 2>&1 | sudo tee /dev/tty1",
    ))?;
    println!("{}", proof.trim());
    if !proof.contains("the receipt verifies") {
        bail!("verify-receipt did not confirm the target:\n{proof}");
    }
    if proof.lines().filter(|l| l.ends_with(": OK")).count() == 0 {
        bail!("no files verified against the manifest:\n{proof}");
    }
    Ok(())
}

/// Migrate a partitioned source onto a larger target, then finalize: grow the partition and its
/// filesystem to fill the new disk, and check it is clean with every file intact.
fn migrate_reshape(
    source: &mut Guest,
    target: &mut Guest,
    src_dev: &str,
    tgt_dev: &str,
    mig_port: u16,
    data_mib: u64,
) -> Result<()> {
    println!("migrating the partitioned source onto the larger target…");
    let sent = source.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate send --source {src_dev} --to 10.0.2.2:{mig_port}"
    ))?;
    if !sent.contains("copied and verified") {
        bail!("the sender did not report success:\n{sent}");
    }
    let recv_log = target.ssh(
        "for i in $(seq 1 30); do systemctl is-active migrate-recv >/dev/null 2>&1 || break; sleep 1; done; \
         cat /tmp/recv.log 2>/dev/null | tail -3",
    )?;
    if !recv_log.contains("written and verified") {
        bail!("the receiver did not report a verified target:\n{recv_log}");
    }

    println!("finalizing: growing the partition and filesystem to fill the new disk…");
    let fin = target.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate finalize --data {tgt_dev} --grow-last 2>&1 | grep -v '^Warning:'"
    ))?;
    println!("{}", fin.trim());
    if !fin.contains("finalize: done") {
        bail!("finalize did not complete:\n{fin}");
    }

    let check = target.ssh(&format!(
        "p=$(lsblk -nro NAME,TYPE {tgt_dev} | awk '$2==\"part\"{{print \"/dev/\"$1}}' | tail -1); \
         sudo e2fsck -fn $p >/dev/null 2>&1 && echo FSCK-CLEAN; \
         b=$(sudo dumpe2fs -h $p 2>/dev/null | awk -F: '/Block count/{{gsub(/ /,\"\",$2);print $2}}'); \
         echo GROWN_MIB=$((b*4/1024)); \
         sudo mkdir -p /mnt/d && sudo mount $p /mnt/d && (cd /mnt/d && sudo sha256sum --quiet -c MANIFEST && echo FILES-OK); sudo umount /mnt/d",
    ))?;
    println!("verify: {}", check.replace('\n', " ").trim());
    if !check.contains("FSCK-CLEAN") || !check.contains("FILES-OK") {
        bail!("the grown filesystem is not clean or files differ:\n{check}");
    }
    let grown_mib: u64 = check
        .lines()
        .find_map(|l| l.strip_prefix("GROWN_MIB="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if grown_mib <= data_mib {
        bail!("the filesystem did not grow past the original {data_mib} MiB (got {grown_mib} MiB)");
    }
    Ok(())
}

/// LVM mirror mode: the source mirrors its volume onto the target's PV over nvme-tcp, syncs the
/// raid1 legs, stands down, and the target promotes its leg to a standalone volume. The source's
/// LVM metadata is written (the M1 exception). Nothing touches a real disk.
fn migrate_mirror_rig(paths: &Paths, data_mib: u64) -> Result<()> {
    let migrate_bin = build_musl_migrate(paths)?;
    let data_bytes = data_mib * (1 << 20);
    let src_root = overlay_of(paths, &paths.prepared(), "mig-src.qcow2")?;
    let tgt_root = overlay_of(paths, &paths.prepared(), "mig-tgt.qcow2")?;
    let src_data = paths.out.join("mig-src-data.raw");
    let tgt_data = paths.out.join("mig-tgt-data.raw");
    create_raw(&src_data, data_bytes)?;
    create_raw(&tgt_data, data_bytes)?;

    let display = display_from_env();
    let nvme_port = guest::free_port()?;

    println!("booting the target guest (nvme-tcp export)…");
    let mut target = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &tgt_root,
            display,
            extra_disks: &[ExtraDisk {
                image: tgt_data.clone(),
                serial: "tgtdata".into(),
            }],
            extra_hostfwd: &[(nvme_port, 4420)],
        },
    )?;
    target.wait_ssh()?;
    println!("booting the source guest (LVM mirror)…");
    let mut source = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &src_root,
            display,
            extra_disks: &[ExtraDisk {
                image: src_data.clone(),
                serial: "srcdata".into(),
            }],
            extra_hostfwd: &[],
        },
    )?;
    source.wait_ssh()?;

    let src_dev = "/dev/disk/by-id/virtio-srcdata";
    let tgt_dev = "/dev/disk/by-id/virtio-tgtdata";

    // Ubuntu cloud images keep the nvme-target and nvme-tcp modules in linux-modules-extra, and
    // the prepared image's 2 GiB root is nearly full. Grow the root into the gap the prepared
    // layout leaves after it, then install the modules (version-matched to the running kernel).
    // Ubuntu cloud images keep the nvme-target and nvme-tcp modules in linux-modules-extra, and the
    // prepared image's 2 GiB root is nearly full. Grow the root into the gap the prepared layout
    // leaves after it, free the apt cache, and install the modules version-matched to the kernel.
    // The linux-image initramfs trigger may fail for lack of space, which is harmless here: only the
    // module files and depmod matter (nothing reboots), so depmod then makes modprobe find them.
    // The prepared image's 2 GiB root is nearly full, and Ubuntu keeps the nvme-tcp and nvme-target
    // modules in linux-modules-extra. Grow the root into the gap the prepared layout leaves after it
    // (the live-mounted disk can't be repartitioned, but it can grow into a trailing gap), free the
    // apt cache and snapd, then extract just the module files with dpkg-deb -x (no maintainer
    // scripts, so the linux-image initramfs trigger can't fill the disk) and depmod them in.
    println!("growing the root and installing lvm2, nvme-cli, the nvme modules…");
    for guest in [&mut source, &mut target] {
        guest.ssh(
            "sudo growpart /dev/sda 1 2>/dev/null; sudo partx -u /dev/sda 2>/dev/null; \
             sudo resize2fs /dev/sda1 2>/dev/null; sudo apt-get clean; true",
        )?;
        guest.ssh("sudo apt-get update -qq; sudo apt-get install -y -qq lvm2 nvme-cli 2>&1 | tail -1; true")?;
        // Extract ONLY the nvme module files (about a megabyte) from the 64 MiB package, not all of
        // linux-modules-extra (230 MiB would fill the small root). Retry the download (it crosses a
        // slow NAT) and verify the target module actually landed, so a partial download can't pass.
        guest
            .ssh(
                "cd /tmp && rm -f linux-modules-extra-*.deb; k=$(uname -r); \
             for t in 1 2 3 4; do apt-get download linux-modules-extra-$k && break; sleep 2; done; \
             dpkg-deb --fsys-tarfile linux-modules-extra-*.deb \
               | sudo tar -x -C / --wildcards './lib/modules/*/kernel/drivers/nvme/*'; \
             rm -f linux-modules-extra-*.deb; sudo depmod -a; \
             test -f /lib/modules/$k/kernel/drivers/nvme/target/nvmet-tcp.ko \
               && test -f /lib/modules/$k/kernel/drivers/nvme/host/nvme-tcp.ko && echo MODULES-OK",
            )
            .context("extracting the nvme-tcp modules into the guest")?;
        guest.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
        guest
            .ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;
    }
    // Load by the leaf module so modprobe pulls each dependency itself (nvmet_tcp pulls nvmet;
    // nvme_tcp pulls nvme_fabrics and nvme_core).
    target
        .ssh(
            "sudo depmod -a; sudo modprobe nvmet_tcp && lsmod | grep -q nvmet_tcp && echo NVMET-OK",
        )
        .context("loading nvmet_tcp on the target")?;
    source
        .ssh("sudo depmod -a; sudo modprobe nvme_tcp && sudo modprobe dm_raid && lsmod | grep -q nvme_tcp && echo NVME-OK")
        .context("loading nvme_tcp on the source")?;

    let lv_mib = (data_mib / 2).max(16);
    let file_mib = (lv_mib / 4).max(2);
    println!("building the source LVM volume ({lv_mib} MiB) with checksummed files…");
    source.ssh(&format!(
        "sudo pvcreate -y {src_dev} && sudo vgcreate vgsrc {src_dev} \
         && sudo lvcreate -y -L {lv_mib}M -n lvdata vgsrc \
         && sudo mkfs.ext4 -q -F /dev/vgsrc/lvdata && sudo mkdir -p /mnt/d \
         && sudo mount /dev/vgsrc/lvdata /mnt/d \
         && for i in 1 2 3; do sudo dd if=/dev/urandom of=/mnt/d/file$i bs=1M count={file_mib} status=none; done \
         && sudo sh -c 'cd /mnt/d && sha256sum file* > MANIFEST' && sudo sync"
    ))?;

    println!("exporting the target PV over nvme-tcp…");
    target.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate mirror-export --dev {tgt_dev}"
    ))?;

    println!("attaching the mirror leg on the source and syncing…");
    let attach = source.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate mirror-attach --addr 10.0.2.2 --port {nvme_port} --vg vgsrc --lv lvdata"
    ))?;
    println!("{}", attach.trim());
    if !attach.contains("in sync") {
        bail!("the mirror did not reach sync:\n{attach}");
    }

    println!("cutover: unmount the source, stand it down, and promote the target…");
    source.ssh(
        "sudo umount /mnt/d && sudo /usr/local/sbin/bootshift-migrate mirror-stand-down --vg vgsrc",
    )?;
    let verify = target.ssh(
        "sudo pvscan --cache >/dev/null 2>&1; sudo vgscan >/dev/null 2>&1; \
         sudo /usr/local/sbin/bootshift-migrate mirror-promote --vg vgsrc --lv lvdata \
         && sudo mkdir -p /mnt/d && sudo mount /dev/vgsrc/lvdata /mnt/d \
         && (cd /mnt/d && sudo sha256sum --quiet -c MANIFEST && echo FILES-OK); sudo umount /mnt/d 2>/dev/null || true",
    )?;
    println!("{}", verify.trim());
    if !verify.contains("FILES-OK") {
        bail!("the promoted target volume did not verify:\n{verify}");
    }

    source.poweroff()?;
    target.poweroff()?;
    println!();
    println!("MIRROR RIG PASSED");
    println!("  built an LVM volume on the source and mirrored it onto the target over nvme-tcp");
    println!("  synced the raid1 legs, stood the source down, and promoted the target leg");
    println!("  the promoted target volume mounts with every file intact");
    Ok(())
}

/// Rehearsal: migrate a disk, then take a writable dm-snapshot of the replica and prove it is a
/// testable copy of the migrated data whose own writes never reach the replica.
fn migrate_rehearse_rig(paths: &Paths, data_mib: u64) -> Result<()> {
    let migrate_bin = build_musl_migrate(paths)?;
    let data_bytes = data_mib * (1 << 20);
    let scratch_bytes = 64u64 * (1 << 20);

    let src_root = overlay_of(paths, &paths.prepared(), "mig-src.qcow2")?;
    let tgt_root = overlay_of(paths, &paths.prepared(), "mig-tgt.qcow2")?;
    let src_data = paths.out.join("mig-src-data.raw");
    let tgt_data = paths.out.join("mig-tgt-data.raw");
    let tgt_scratch = paths.out.join("mig-tgt-scratch.raw");
    let tgt_cow = paths.out.join("mig-tgt-cow.raw");
    create_raw(&src_data, data_bytes)?;
    create_raw(&tgt_data, data_bytes)?;
    create_raw(&tgt_scratch, scratch_bytes)?;
    create_raw(&tgt_cow, data_bytes)?;

    let display = display_from_env();
    let mig_port = guest::free_port()?;

    println!("booting the target guest (receiver + rehearsal)…");
    let mut target = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &tgt_root,
            display,
            extra_disks: &[
                ExtraDisk {
                    image: tgt_data.clone(),
                    serial: "tgtdata".into(),
                },
                ExtraDisk {
                    image: tgt_scratch.clone(),
                    serial: "tgtscr".into(),
                },
                ExtraDisk {
                    image: tgt_cow.clone(),
                    serial: "tgtcow".into(),
                },
            ],
            extra_hostfwd: &[(mig_port, 9876)],
        },
    )?;
    target.wait_ssh()?;
    println!("booting the source guest (the sender)…");
    let mut source = Guest::launch_opts(
        paths,
        &LaunchOpts {
            disk: &src_root,
            display,
            extra_disks: &[ExtraDisk {
                image: src_data.clone(),
                serial: "srcdata".into(),
            }],
            extra_hostfwd: &[],
        },
    )?;
    source.wait_ssh()?;

    let src_dev = "/dev/disk/by-id/virtio-srcdata";
    let tgt_dev = "/dev/disk/by-id/virtio-tgtdata";
    let scr_dev = "/dev/disk/by-id/virtio-tgtscr";
    let cow_dev = "/dev/disk/by-id/virtio-tgtcow";

    println!("filling the source with a filesystem and checksummed files…");
    source.ssh(&format!("sudo mkfs.ext4 -q -F {src_dev}"))?;
    source.ssh(&format!(
        "sudo mkdir -p /mnt/d && sudo mount {src_dev} /mnt/d \
         && for i in $(seq 1 {}); do sudo dd if=/dev/urandom of=/mnt/d/file$i bs=1M count=8 status=none; done \
         && sudo sh -c 'cd /mnt/d && sha256sum file* > MANIFEST' && sudo sync && sudo umount /mnt/d",
        (data_mib / 16).max(1),
    ))?;

    println!("installing the migrate binary in both guests…");
    source.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    source.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;
    target.scp(&migrate_bin, "/tmp/bootshift-migrate")?;
    target.ssh("sudo install -m0755 /tmp/bootshift-migrate /usr/local/sbin/bootshift-migrate")?;

    println!("migrating the source to the replica…");
    target.ssh(&format!(
        "sudo systemctl reset-failed migrate-recv 2>/dev/null; true; \
         sudo systemd-run --unit=migrate-recv --collect /bin/sh -c \
         'exec /usr/local/sbin/bootshift-migrate receive --data {tgt_dev} --scratch {scr_dev} \
          --listen 0.0.0.0:9876 > /tmp/recv.log 2>&1'"
    ))?;
    target.ssh("for i in $(seq 1 30); do ss -ltn 2>/dev/null | grep -q ':9876' && exit 0; sleep 0.5; done; exit 1")
        .context("the receiver never started listening")?;
    let sent = source.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate send --source {src_dev} --to 10.0.2.2:{mig_port}"
    ))?;
    if !sent.contains("copied and verified") {
        bail!("the sender did not report success:\n{sent}");
    }
    target.ssh("for i in $(seq 1 30); do systemctl is-active migrate-recv >/dev/null 2>&1 || break; sleep 1; done; true")?;

    println!("rehearsing: snapshot the replica and test it…");
    target.ssh("sudo modprobe dm_snapshot 2>&1; true")?;
    let rehearse = target.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate rehearse --data {tgt_dev} --cow {cow_dev} --name reh \
         && sudo mkdir -p /mnt/snap && sudo mount /dev/mapper/reh-snap /mnt/snap \
         && (cd /mnt/snap && sudo sha256sum --quiet -c MANIFEST && echo SNAP-FILES-OK) \
         && echo 'rehearsal-only change' | sudo tee /mnt/snap/rehearsal-note >/dev/null \
         && sudo umount /mnt/snap"
    ))?;
    println!("{}", rehearse.trim());
    if !rehearse.contains("SNAP-FILES-OK") {
        bail!("the rehearsal snapshot did not carry the migrated files:\n{rehearse}");
    }

    println!("ending the rehearsal and checking the replica is untouched…");
    let verify = target.ssh(&format!(
        "sudo /usr/local/sbin/bootshift-migrate rehearse --data {tgt_dev} --name reh --end \
         && sudo mkdir -p /mnt/d && sudo mount {tgt_dev} /mnt/d \
         && (cd /mnt/d && sudo sha256sum --quiet -c MANIFEST && echo REPLICA-CLEAN) \
         && (test ! -e /mnt/d/rehearsal-note && echo NO-REHEARSAL-LEAK); sudo umount /mnt/d"
    ))?;
    println!("{}", verify.trim());
    if !verify.contains("REPLICA-CLEAN") || !verify.contains("NO-REHEARSAL-LEAK") {
        bail!("the replica was disturbed by the rehearsal:\n{verify}");
    }

    source.poweroff()?;
    target.poweroff()?;
    println!();
    println!("REHEARSE RIG PASSED");
    println!("  migrated a {data_mib} MiB disk, then snapshotted the replica");
    println!("  the snapshot carried every migrated file and was writable");
    println!("  the rehearsal's own write never reached the replica");
    Ok(())
}

/// The sha256 of a whole block device in the guest.
fn device_sha(guest: &mut Guest, dev: &str) -> Result<String> {
    let out = guest.ssh(&format!("sudo sha256sum {dev}"))?;
    Ok(out.split_whitespace().next().unwrap_or("").to_string())
}

/// The dynamic bootshift-migrate binary with the eBPF tracker, for live mode.
fn build_migrate_ebpf(paths: &Paths) -> Result<PathBuf> {
    println!("building bootshift-migrate with the eBPF tracker…");
    run(Command::new("cargo").current_dir(&paths.repo).args([
        "build",
        "-p",
        "bootshift-migrate-cli",
        "--features",
        "ebpf",
        "--release",
    ]))?;
    Ok(paths.repo.join("target/release/bootshift-migrate"))
}

/// The static bootshift-migrate binary built for musl.
fn build_musl_migrate(paths: &Paths) -> Result<PathBuf> {
    println!("building the static bootshift-migrate binary…");
    run(Command::new("cargo").current_dir(&paths.repo).args([
        "build",
        "-p",
        "bootshift-migrate-cli",
        "--release",
        "--target",
        "x86_64-unknown-linux-musl",
    ]))?;
    Ok(paths
        .repo
        .join("target/x86_64-unknown-linux-musl/release/bootshift-migrate"))
}

/// Creates a raw image file of `bytes` bytes, replacing any existing one.
fn create_raw(path: &Path, bytes: u64) -> Result<()> {
    let _ = std::fs::remove_file(path);
    run(Command::new("qemu-img")
        .args(["create", "-f", "raw"])
        .arg(path)
        .arg(bytes.to_string()))?;
    Ok(())
}

/// The guest's one disk, whatever the controller calls it.
fn guest_disk(guest: &mut Guest) -> Result<String> {
    let path = guest.ssh(
        "lsblk --nodeps --noheadings --raw --output PATH,TYPE | awk '$2==\"disk\"{print $1; exit}'",
    )?;
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
    run(Command::new("cargo").current_dir(&paths.repo).args([
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
