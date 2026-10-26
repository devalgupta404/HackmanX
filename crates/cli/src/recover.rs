//! The recovery system: `bootshift` running as PID 1 from the initramfs, and the same work
//! started by hand from a live USB.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bootshift_boot::cmdline::RecoveryArgs;
use bootshift_boot::initramfs::MODULE_LIST_PATH;
use bootshift_core::layout::Guid;
use bootshift_disk::{Access, RealDevice};
use bootshift_engine::copier::{CopyOptions, ProgressSink};
use bootshift_engine::recovery;
use bootshift_fs::run::{CommandLine, Runner, SystemRunner};
use rustix::mount::{MountFlags, mount};
use rustix::system::{RebootCommand, reboot};

const DISK_WAIT: Duration = Duration::from_secs(60);
const WORK_DIR: &str = "/run/bootshift";
/// How long the error screen waits for Enter before powering off on its own, in case the
/// keyboard modules never loaded.
const ERROR_SCREEN_WAIT: Duration = Duration::from_secs(300);

/// Runs as the initramfs's init. Never returns: it reboots when the work is done, and powers
/// off after showing the problem if it can't finish. It never boots an operating system over
/// a half-finished disk.
pub fn run_as_init() -> ! {
    say("BootShift recovery: rearranging your disk.");
    say("If the power goes out, just turn the computer back on. BootShift will continue.");

    let outcome = prepare_system().and_then(|args| {
        let disk = wait_for_disk(args.disk_guid)?;
        say(&format!("Working on {}.", disk.display()));
        recovery::run(
            &SystemRunner,
            &disk,
            &args,
            Path::new(WORK_DIR),
            &CopyOptions::default(),
            &mut ConsoleProgress::default(),
        )
        .map_err(Into::into)
    });

    match outcome {
        Ok(()) => {
            say("Done. Restarting into your system.");
            rustix::fs::sync();
            let _ = reboot(RebootCommand::Restart);
        }
        Err(error) => {
            let message = format!("{error:#}");
            say(&format!("BootShift stopped: {message}"));
            say("Your data is safe: the journal records exactly where the work stopped.");
            say("Do not start Windows or another system on this disk yet.");
            if message.contains("can't be read") {
                say("A block of the disk is unreadable. To copy past it, losing only that");
                say("block, run from a live USB:  bootshift resume --accept-unreadable …");
            }
            say("Press Enter to power off; it powers off by itself in five minutes.");
            say("Turning the computer on again will retry.");
            wait_for_enter_or(ERROR_SCREEN_WAIT);
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
pub fn resume(disk: &Path, kernel_args: &str, accept_unreadable: bool) -> anyhow::Result<()> {
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
    let options = CopyOptions {
        accept_unreadable,
        ..CopyOptions::default()
    };
    recovery::run(
        &SystemRunner,
        disk,
        &args,
        Path::new(WORK_DIR),
        &options,
        &mut ConsoleProgress::default(),
    )?;
    println!("The run is finished and normal booting is restored.");
    Ok(())
}

/// Prints each operation and a line every few percent, slowly enough for a serial console.
#[derive(Default)]
struct ConsoleProgress {
    last_twentieth: Option<u64>,
}

impl ProgressSink for ConsoleProgress {
    fn operation(&mut self, index: u32, count: u32, what: &str) {
        self.last_twentieth = None;
        say(&format!("[{index}/{count}] {what}"));
    }

    fn copied(&mut self, done: u64, total: u64) {
        if total == 0 {
            return;
        }
        let twentieth = done * 20 / total;
        if Some(twentieth) != self.last_twentieth {
            self.last_twentieth = Some(twentieth);
            say(&format!(
                "  {:>3}%  {} of {} moved and verified",
                done * 100 / total,
                gib(done),
                gib(total)
            ));
        }
    }
}

fn gib(bytes: u64) -> String {
    if bytes < 1 << 30 {
        format!("{:.0} MiB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{:.1} GiB", bytes as f64 / (1 << 30) as f64)
    }
}

/// Returns when Enter is pressed, or after `timeout` if it never is.
fn wait_for_enter_or(timeout: Duration) {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().lock().lines().next();
        let _ = sender.send(());
    });
    let _ = receiver.recv_timeout(timeout);
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

/// In the recovery system, writing through /dev/kmsg puts the line on every console the
/// kernel knows (the screen and the serial port) exactly once. Elsewhere, stderr.
///
/// /dev/kmsg makes one log record per write(2), so the whole line, priority prefix included,
/// must go in a single write — writeln!'s several writes would split it into empty records.
fn say(line: &str) {
    use std::io::Write;
    let record = format!("<2>bootshift: {line}\n");
    let printk = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/kmsg")
        .and_then(|mut kmsg| kmsg.write_all(record.as_bytes()));
    if printk.is_err() {
        eprintln!("{line}");
    }
}
