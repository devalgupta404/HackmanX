//! The recovery system: `bootshift` running as PID 1 from the initramfs, and the same work
//! started by hand from a live USB.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bootshift_boot::cmdline::RecoveryArgs;
use bootshift_boot::initramfs::MODULE_LIST_PATH;
use bootshift_core::layout::Guid;
use bootshift_disk::{Access, RealDevice};
use bootshift_engine::copier::{CopyOptions, MAX_CHUNK, ProgressSink};
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

/// The recovery screen, drawn on the console to match the app's design: a dark screen, a bright
/// title, a green block progress bar, the count of what has moved, and the safety message. The
/// whole block is redrawn in place on each update (cursor home, each line erased to its end), so
/// it updates without scrolling or flicker. Colours are the 16-colour ANSI set, which every
/// Linux console renders.
#[derive(Default)]
struct ConsoleProgress {
    out: Vec<std::fs::File>,
    opened: bool,
    cleared: bool,
    last_percent: Option<u64>,
}

const BAR_CELLS: u64 = 36;
const RULE_WIDTH: usize = 60;

/// The screen (`/dev/tty0`, the VGA console in the VM window) and the system console
/// (`/dev/console`, which is the serial port when the kernel has `console=ttyS0`). Writing to
/// both puts the recovery screen where a person is watching and where the test harness reads.
const CONSOLES: [&str; 2] = ["/dev/tty0", "/dev/console"];

impl ConsoleProgress {
    fn write(&mut self, text: &str) {
        if !self.opened {
            self.opened = true;
            self.out = CONSOLES
                .iter()
                .filter_map(|path| std::fs::OpenOptions::new().write(true).open(path).ok())
                .collect();
        }
        if self.out.is_empty() {
            eprint!("{text}");
            let _ = std::io::stderr().flush();
            return;
        }
        for file in &mut self.out {
            let _ = file.write_all(text.as_bytes());
        }
    }
}

/// Builds the whole recovery screen for the run as it stands. Starts at the cursor home position
/// and erases each line to its end, so writing it over the previous frame updates in place.
fn screen(done: u64, total: u64) -> String {
    let percent = (done * 100).checked_div(total).unwrap_or(0).min(100);
    let filled = (percent * BAR_CELLS / 100).min(BAR_CELLS) as usize;
    let bar = format!(
        "\x1b[92m{}\x1b[90m{}\x1b[0m",
        "█".repeat(filled),
        "░".repeat(BAR_CELLS as usize - filled),
    );
    let chunks = total.div_ceil(MAX_CHUNK).max(1);
    let chunk_line = if total > 0 && done >= total {
        format!("all {chunks} chunks copied, flushed, verified, journaled")
    } else {
        let at = done.div_ceil(MAX_CHUNK).clamp(1, chunks);
        format!("chunk {at} of {chunks} copied, flushed, verified, journaled")
    };
    let rule = format!("\x1b[90m{}\x1b[0m", "─".repeat(RULE_WIDTH));
    let rows = [
        String::new(),
        "\x1b[1;97mBootShift recovery\x1b[0m".to_string(),
        "\x1b[90mmoving your partitions, before any operating system starts\x1b[0m".to_string(),
        String::new(),
        rule.clone(),
        String::new(),
        format!("{bar}  \x1b[1;97m{percent:>3}%\x1b[0m"),
        format!("\x1b[37m{} of {} moved\x1b[0m", gib(done), gib(total)),
        format!("\x1b[90m{chunk_line}\x1b[0m"),
        String::new(),
        rule,
        String::new(),
        "\x1b[92mPower cut? Rude, but fine. Turn the computer back on.\x1b[0m".to_string(),
        "\x1b[92mBootShift resumes from the last verified chunk. No drama.\x1b[0m".to_string(),
        "\x1b[93mDo not boot Windows from the firmware menu. A half-moved disk\x1b[0m".to_string(),
        "\x1b[93mis not something it handles gracefully.\x1b[0m".to_string(),
    ];
    let mut out = String::from("\x1b[H");
    for row in rows {
        // Two-space margin, then erase the rest of the line so a shorter frame leaves no trail.
        out.push_str("  ");
        out.push_str(&row);
        out.push_str("\x1b[K\n");
    }
    // Erase anything the previous, taller frame may have left below.
    out.push_str("\x1b[J");
    out
}

impl ProgressSink for ConsoleProgress {
    fn operation(&mut self, _index: u32, _count: u32, _what: &str) {
        // A new operation restarts the bar; force the next `copied` to redraw.
        self.last_percent = None;
    }

    fn copied(&mut self, done: u64, total: u64) {
        if total == 0 {
            return;
        }
        let percent = (done * 100 / total).min(100);
        if self.last_percent == Some(percent) {
            return;
        }
        self.last_percent = Some(percent);
        let mut frame = String::new();
        if !self.cleared {
            // Black background, cleared once; later frames just redraw from the home position.
            self.cleared = true;
            frame.push_str("\x1b[40m\x1b[2J");
        }
        frame.push_str(&screen(done, total));
        self.write(&frame);
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

/// A discrete message on the machine's console (the intro, the final line, errors). Each lands
/// on its own line, on both the screen and the system console. Before devtmpfs is mounted the
/// device nodes don't exist yet, so it falls back to stderr, which is the console for PID 1.
fn say(line: &str) {
    let text = format!("\n  {line}");
    let mut reached = false;
    for path in CONSOLES {
        if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = console.write_all(text.as_bytes());
            reached = true;
        }
    }
    if !reached {
        eprintln!("{line}");
    }
}
