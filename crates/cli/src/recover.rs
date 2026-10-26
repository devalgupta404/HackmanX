//! The recovery system: `bootshift` running as PID 1 from the initramfs, and the same work
//! started by hand from a live USB.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bootshift_boot::cmdline::RecoveryArgs;
use bootshift_boot::initramfs::MODULE_LIST_PATH;
use bootshift_core::layout::Guid;
use bootshift_disk::{Access, RealDevice};
use bootshift_engine::recovery;
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};
use rustix::mount::{MountFlags, mount};
use rustix::system::{RebootCommand, reboot};

const DISK_WAIT: Duration = Duration::from_secs(60);
const WORK_DIR: &str = "/run/bootshift";

/// Runs as the initramfs's init. Never returns: it reboots when the work is done, and powers
/// off after showing the problem if it can't finish. It never boots an operating system over
/// a half-finished disk.
pub fn run_as_init() -> ! {
    say("BootShift is rearranging your disk.");
    say("If the power goes out, just turn the computer back on. BootShift will continue.");

    let outcome = prepare_system().and_then(|args| {
        let disk = wait_for_disk(args.disk_guid)?;
        say(&format!("Working on {}.", disk.display()));
        recovery::run(&SystemRunner, &disk, &args, Path::new(WORK_DIR)).map_err(Into::into)
    });

    match outcome {
        Ok(()) => {
            say("Done. Restarting into your system.");
            rustix::fs::sync();
            let _ = reboot(RebootCommand::Restart);
        }
        Err(error) => {
            say(&format!("BootShift stopped: {error:#}"));
            say("Your data is safe: the journal records exactly where the work stopped.");
            say("Do not start Windows or another system on this disk yet.");
            say("Press Enter to power off. Turning the computer on again will retry.");
            let _ = std::io::stdin().lock().lines().next();
            rustix::fs::sync();
            let _ = reboot(RebootCommand::PowerOff);
        }
    }
    loop {
        sleep(Duration::from_secs(60));
    }
}

/// Finishes a run by hand from a live system, using the same kernel options the recovery
/// system was given (saved as `EFI/bootshift/recovery-args.txt` on the ESP).
pub fn resume(disk: &Path, kernel_args: &str) -> anyhow::Result<()> {
    let args = RecoveryArgs::from_cmdline(kernel_args)?;
    let device = RealDevice::open(disk, Access::ReadOnly)
        .with_context(|| format!("opening {}", disk.display()))?;
    if !recovery::disk_matches(&device, args.disk_guid) {
        bail!(
            "{} is not the disk this run was planned for",
            disk.display()
        );
    }
    drop(device);
    recovery::run(&SystemRunner, disk, &args, Path::new(WORK_DIR))?;
    println!("The run is finished and normal booting is restored.");
    Ok(())
}

fn prepare_system() -> anyhow::Result<RecoveryArgs> {
    for (source, target, fs_type) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
        ("tmpfs", "/run", "tmpfs"),
        ("efivarfs", "/sys/firmware/efi/efivars", "efivarfs"),
    ] {
        mount(source, target, fs_type, MountFlags::empty(), None)
            .with_context(|| format!("mounting {target}"))?;
    }
    load_modules();
    let cmdline =
        std::fs::read_to_string("/proc/cmdline").context("reading the kernel command line")?;
    Ok(RecoveryArgs::from_cmdline(&cmdline)?)
}

fn load_modules() {
    let list = std::fs::read_to_string(MODULE_LIST_PATH).unwrap_or_default();
    for module in list.lines().filter(|m| !m.is_empty()) {
        let _ = SystemRunner.run(&CommandLine::new("modprobe").arg(module));
    }
}

/// Finds the disk whose GPT has `guid`, waiting while the kernel discovers drives.
fn wait_for_disk(guid: Guid) -> anyhow::Result<PathBuf> {
    let started = Instant::now();
    loop {
        if let Some(disk) = find_disk(guid) {
            return Ok(disk);
        }
        if started.elapsed() > DISK_WAIT {
            bail!(
                "no disk with GUID {guid} appeared within {} seconds",
                DISK_WAIT.as_secs()
            );
        }
        sleep(Duration::from_secs(1));
    }
}

fn find_disk(guid: Guid) -> Option<PathBuf> {
    let entries = std::fs::read_dir("/sys/block").ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("loop") || name.starts_with("ram") {
            return None;
        }
        let path = PathBuf::from("/dev").join(&name);
        let device = RealDevice::open(&path, Access::ReadOnly).ok()?;
        recovery::disk_matches(&device, guid).then_some(path)
    })
}

fn say(line: &str) {
    eprintln!("{line}");
}
