//! A throwaway QEMU guest: UEFI firmware, the disk as NVMe, cloud-init for first-boot setup,
//! SSH through a forwarded localhost port, and the serial console captured to a file.

use std::io::{Read, Seek, SeekFrom};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub struct Paths {
    pub repo: PathBuf,
    pub out: PathBuf,
    pub base_image: PathBuf,
    pub ovmf_code: PathBuf,
    pub ovmf_vars: PathBuf,
    pub ssh_key: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("xtask has no parent directory")?
            .to_path_buf();
        let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
        let base_image = match std::env::var_os("BOOTSHIFT_BASE_IMAGE") {
            Some(path) => PathBuf::from(path),
            None => home.join("vm/jammy-server-cloudimg-amd64.img"),
        };
        if !base_image.exists() {
            bail!(
                "no base image at {}; download it with:\n  wget -P ~/vm https://cloud-images.ubuntu.com/jammy/current/jammy-server-cloudimg-amd64.img",
                base_image.display()
            );
        }
        Ok(Self {
            out: repo.join("xtask/out"),
            base_image,
            ovmf_code: first_existing(&[
                "/usr/share/OVMF/OVMF_CODE_4M.fd",
                "/usr/share/OVMF/OVMF_CODE.fd",
            ])?,
            ovmf_vars: first_existing(&[
                "/usr/share/OVMF/OVMF_VARS_4M.fd",
                "/usr/share/OVMF/OVMF_VARS.fd",
            ])?,
            ssh_key: home.join(".ssh/id_ed25519"),
            repo,
        })
    }

    pub fn prepared(&self) -> PathBuf {
        self.out.join("prepared.qcow2")
    }

    pub fn musl_binary(&self) -> PathBuf {
        self.repo
            .join("target/x86_64-unknown-linux-musl/release/bootshift")
    }
}

fn first_existing(candidates: &[&str]) -> Result<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .with_context(|| format!("none of {candidates:?} exists; install the ovmf package"))
}

/// The NoCloud seed: authorizes our SSH key and installs the packages the engine needs.
pub fn write_seed(paths: &Paths) -> Result<()> {
    std::fs::create_dir_all(&paths.out)?;
    let public_key = std::fs::read_to_string(paths.ssh_key.with_extension("pub"))
        .with_context(|| format!("reading {}.pub", paths.ssh_key.display()))?;
    // growpart off: the root partition must not swallow the disk, the data partition and the
    // move gap need the space.
    let user_data = format!(
        "#cloud-config\nssh_authorized_keys:\n  - {}\ngrowpart:\n  mode: \"off\"\nresize_rootfs: false\npackage_update: true\npackages: [zstd, ntfs-3g, smartmontools, efibootmgr, gdisk, btrfs-progs, lvm2, nvme-cli, cloud-guest-utils]\n",
        public_key.trim()
    );
    std::fs::write(paths.out.join("user-data"), user_data)?;
    std::fs::write(
        paths.out.join("meta-data"),
        "instance-id: bootshift-vm\nlocal-hostname: bootshift-vm\n",
    )?;
    let mut command = Command::new("cloud-localds");
    command
        .arg(paths.out.join("seed.iso"))
        .arg(paths.out.join("user-data"))
        .arg(paths.out.join("meta-data"));
    let status = command.status().context("running cloud-localds")?;
    if !status.success() {
        bail!("cloud-localds failed with {status}");
    }
    Ok(())
}

/// An extra disk to attach to a guest, as a virtio block device. Inside the guest it appears at
/// `/dev/disk/by-id/virtio-<serial>`, a stable name independent of probe order.
pub struct ExtraDisk {
    pub image: PathBuf,
    pub serial: String,
}

/// Everything a launch may vary: the root disk, whether to show a window, extra data disks, and
/// extra host-to-guest port forwards (host port, guest port) on top of SSH.
pub struct LaunchOpts<'a> {
    pub disk: &'a Path,
    pub display: bool,
    pub extra_disks: &'a [ExtraDisk],
    pub extra_hostfwd: &'a [(u16, u16)],
}

pub struct Guest {
    child: Option<Child>,
    ssh_port: u16,
    ssh_key: PathBuf,
    serial: PathBuf,
}

impl Guest {
    pub fn launch(paths: &Paths, disk: &Path, display: bool) -> Result<Self> {
        Self::launch_opts(paths, &LaunchOpts {
            disk,
            display,
            extra_disks: &[],
            extra_hostfwd: &[],
        })
    }

    pub fn launch_opts(paths: &Paths, opts: &LaunchOpts) -> Result<Self> {
        let disk = opts.disk;
        let vars = disk.with_extension("vars.fd");
        if !vars.exists() {
            std::fs::copy(&paths.ovmf_vars, &vars)
                .with_context(|| format!("copying OVMF vars to {}", vars.display()))?;
        }
        let serial = disk.with_extension("serial.log");
        let _ = std::fs::remove_file(&serial);
        let ssh_port = free_port()?;

        let mut command = Command::new("qemu-system-x86_64");
        if Path::new("/dev/kvm").exists() {
            command.arg("-enable-kvm").args(["-cpu", "host"]);
        } else {
            // VT-x off in the BIOS, or no KVM: emulate. Works everywhere, roughly 15x slower.
            eprintln!("warning: /dev/kvm is missing, falling back to TCG emulation (slow)");
            command
                .args(["-accel", "tcg,thread=multi"])
                .args(["-cpu", "max"]);
        }
        let mut netdev = format!("user,id=n0,hostfwd=tcp:127.0.0.1:{ssh_port}-:22");
        for (host, guest_port) in opts.extra_hostfwd {
            netdev.push_str(&format!(",hostfwd=tcp:127.0.0.1:{host}-:{guest_port}"));
        }
        command
            .args(["-machine", "q35"])
            .args(["-smp", "4"])
            .args(["-m", "2048"])
            .arg("-drive")
            .arg(format!(
                "if=pflash,format=raw,readonly=on,file={}",
                paths.ovmf_code.display()
            ))
            .arg("-drive")
            .arg(format!("if=pflash,format=raw,file={}", vars.display()))
            .arg("-drive")
            .arg(format!(
                "file={},if=none,id=root0,format=qcow2,cache=none",
                disk.display()
            ))
            // SATA, not NVMe: this host's QEMU (4.2) rejects the NVMe health log page, and
            // preflight rightly refuses a disk whose SMART can't be read. ATA SMART works.
            .args(["-device", "ide-hd,drive=root0"])
            .arg("-drive")
            .arg(format!(
                "file={},format=raw,if=virtio,readonly=on",
                paths.out.join("seed.iso").display()
            ));
        for extra in opts.extra_disks {
            command
                .arg("-drive")
                .arg(format!(
                    "file={},if=none,id={},format=raw,cache=none",
                    extra.image.display(),
                    extra.serial
                ))
                .args([
                    "-device",
                    &format!(
                        "virtio-blk-pci,drive={0},serial={0}",
                        extra.serial
                    ),
                ]);
        }
        command
            .arg("-netdev")
            .arg(netdev)
            .args(["-device", "virtio-net-pci,netdev=n0"]);
        // With display, the virtual machine's screen opens in a window, for watching a run with
        // your own eyes; the serial log keeps feeding the harness either way.
        if opts.display {
            command.args(["-display", "gtk"]);
        } else {
            command.args(["-display", "none"]);
        }
        command
            .arg("-serial")
            .arg(format!("file:{}", serial.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let child = command.spawn().context("starting qemu-system-x86_64")?;
        Ok(Self {
            child: Some(child),
            ssh_port,
            ssh_key: paths.ssh_key.clone(),
            serial,
        })
    }

    pub fn wait_ssh(&mut self) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(300) {
            if self.try_ssh("true").is_ok() {
                return Ok(());
            }
            // A corrupt root drops the guest to emergency mode before the network comes up, so
            // SSH would never answer. Catch that on the serial console and fail fast, rather than
            // polling for the full timeout, so the caller can rebuild the disk.
            let serial = self.serial_from(0);
            if serial.contains("Reached target Emergency Mode")
                || serial.contains("You are in emergency mode")
            {
                bail!(
                    "the guest dropped to emergency mode, so its root filesystem is corrupt; \
                     serial log tail:\n{}",
                    self.serial_tail()
                );
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        bail!(
            "the guest never answered on SSH; serial log tail:\n{}",
            self.serial_tail()
        );
    }

    /// Runs a command in the guest and returns its stdout. Stderr is part of the error.
    pub fn ssh(&mut self, command: &str) -> Result<String> {
        self.try_ssh(command)
            .with_context(|| format!("in the guest: {command}"))
    }

    fn try_ssh(&mut self, command: &str) -> Result<String> {
        let output = Command::new("ssh")
            .args(["-p", &self.ssh_port.to_string()])
            .arg("-i")
            .arg(&self.ssh_key)
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=4",
                "-o",
                "LogLevel=ERROR",
            ])
            .arg("ubuntu@127.0.0.1")
            .arg(command)
            .output()
            .context("running ssh")?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            bail!(
                "exit {:?}\nstdout: {stdout}\nstderr: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(stdout)
    }

    pub fn scp(&mut self, local: &Path, remote: &str) -> Result<()> {
        let status = Command::new("scp")
            .args(["-P", &self.ssh_port.to_string()])
            .arg("-i")
            .arg(&self.ssh_key)
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
            ])
            .arg(local)
            .arg(format!("ubuntu@127.0.0.1:{remote}"))
            .status()
            .context("running scp")?;
        if !status.success() {
            bail!("scp {} failed with {status}", local.display());
        }
        Ok(())
    }

    pub fn sfdisk_table(&mut self, disk: &str) -> Result<SfdiskTable> {
        let json = self.ssh(&format!("sudo sfdisk --json {disk}"))?;
        let dump: SfdiskDump = serde_json::from_str(&json).context("parsing sfdisk --json")?;
        Ok(dump.partitiontable)
    }

    pub fn ssh_port(&self) -> u16 {
        self.ssh_port
    }

    /// Leaves the virtual machine running after this process ends (for a hand-driven demo):
    /// skips the Drop that would kill it. The child is reparented to init and keeps going.
    pub fn leak(self) {
        std::mem::forget(self);
    }

    pub fn serial_len(&self) -> u64 {
        std::fs::metadata(&self.serial)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    fn serial_from(&self, offset: u64) -> String {
        let Ok(mut file) = std::fs::File::open(&self.serial) else {
            return String::new();
        };
        let _ = file.seek(SeekFrom::Start(offset));
        let mut text = String::new();
        let _ = file.read_to_string(&mut text);
        text
    }

    fn serial_tail(&self) -> String {
        let text = self.serial_from(0);
        let start = text.len().saturating_sub(2000);
        text[start..].to_string()
    }

    /// Waits until `needle` appears on the serial console after `offset`.
    pub fn wait_serial(&mut self, offset: u64, needle: &str, timeout_secs: u64) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(timeout_secs) {
            if self.serial_from(offset).contains(needle) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        bail!(
            "never saw {needle:?} on the serial console; tail:\n{}",
            self.serial_tail()
        );
    }

    /// Waits for a recovery progress line between `min` and `max` percent, and returns it.
    /// Fails if recovery finishes first: the run was too quick to cut.
    pub fn wait_serial_percent(
        &mut self,
        offset: u64,
        min: u32,
        max: u32,
        timeout_secs: u64,
    ) -> Result<u32> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(timeout_secs) {
            let text = self.serial_from(offset);
            for percent in percents(&text) {
                if percent >= min && percent <= max {
                    return Ok(percent);
                }
            }
            if text.contains("Done. Restarting") {
                bail!("recovery finished before the power-cut window ({min}–{max}%)");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!(
            "never saw a progress line in the {min}–{max}% window; tail:\n{}",
            self.serial_tail()
        );
    }

    /// A power cut: the machine is gone this instant.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn poweroff(&mut self) -> Result<()> {
        let _ = self.try_ssh("sudo poweroff");
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            if let Ok(Some(_)) = child.try_wait() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Every `<digits>%` in the text, in order. The recovery console updates its bar in place with
/// `\r` and ANSI escapes, so there are no clean lines to split on; this just reads each percent
/// value wherever a run of digits is followed by `%`.
fn percents(text: &str) -> Vec<u32> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c != '%' {
            continue;
        }
        let mut start = i;
        while start > 0 && chars[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start < i {
            if let Ok(value) = chars[start..i].iter().collect::<String>().parse() {
                out.push(value);
            }
        }
    }
    out
}

pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("finding a free port")?;
    Ok(listener.local_addr()?.port())
}

#[derive(Deserialize)]
struct SfdiskDump {
    partitiontable: SfdiskTable,
}

#[derive(Deserialize)]
pub struct SfdiskTable {
    pub lastlba: Option<u64>,
    #[serde(default)]
    pub partitions: Vec<SfdiskPartition>,
}

#[derive(Deserialize)]
pub struct SfdiskPartition {
    pub node: String,
    pub start: u64,
    pub size: u64,
}
