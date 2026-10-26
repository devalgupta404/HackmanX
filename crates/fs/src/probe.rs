//! Finding disks and partitions with `lsblk`.

use std::path::{Path, PathBuf};

use bootshift_core::layout::{Content, Guid};
use bootshift_core::units::Lba;
use serde::Deserialize;

use crate::parse::unexpected;
use crate::run::{CommandLine, RunError, Runner};

const COLUMNS: &str = "PATH,TYPE,SIZE,LOG-SEC,TRAN,RM,HOTPLUG,PTTYPE,FSTYPE,UUID,LABEL,PARTUUID,PARTTYPE,PARTN,START,MOUNTPOINTS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskInfo {
    pub path: PathBuf,
    pub size: u64,
    pub logical_sector_size: u32,
    pub transport: Option<String>,
    pub removable: bool,
    pub partition_table: Option<String>,
    pub partitions: Vec<PartitionInfo>,
}

impl DiskInfo {
    pub fn is_usb(&self) -> bool {
        self.transport.as_deref() == Some("usb")
    }

    pub fn partition(&self, number: u32) -> Option<&PartitionInfo> {
        self.partitions.iter().find(|p| p.number == number)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionInfo {
    pub path: PathBuf,
    pub number: u32,
    pub start: Lba,
    pub size: u64,
    pub fstype: Option<String>,
    pub uuid: Option<String>,
    pub label: Option<String>,
    pub unique_guid: Option<Guid>,
    pub type_guid: Option<Guid>,
    pub mountpoints: Vec<PathBuf>,
}

impl PartitionInfo {
    pub fn content(&self) -> Content {
        content_of(self.fstype.as_deref(), self.type_guid)
    }

    pub fn is_mounted(&self) -> bool {
        !self.mountpoints.is_empty()
    }
}

/// What BootShift may do with a partition, from its filesystem and partition type.
///
/// A hibernated swap partition (`swsuspend`) is treated as opaque, so it is only ever moved
/// byte for byte and its hibernation image survives.
pub fn content_of(fstype: Option<&str>, type_guid: Option<Guid>) -> Content {
    if type_guid == Some(Guid::EFI_SYSTEM) {
        return Content::EfiSystem;
    }
    match fstype {
        Some("ext4") => Content::Ext4,
        Some("ntfs") => Content::Ntfs,
        Some("btrfs") => Content::Btrfs,
        Some("xfs") => Content::Xfs,
        Some("swap") => Content::Swap,
        Some("BitLocker") => Content::BitLocker,
        _ => Content::Opaque,
    }
}

/// Every disk the kernel knows about, with its partitions. Read-only and safe at any time.
pub fn disks(runner: &dyn Runner) -> Result<Vec<DiskInfo>, RunError> {
    let command = CommandLine::new("lsblk")
        .arg("--json")
        .arg("--bytes")
        .arg("--output")
        .arg(COLUMNS);
    let output = runner.run_ok(&command)?;
    parse_lsblk(&output.text()).map_err(|detail| unexpected(&command, detail))
}

/// What `blkid` finds in the bytes of a device, read directly rather than from udev, which the
/// recovery system doesn't run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    pub fstype: Option<String>,
    pub uuid: Option<String>,
    pub label: Option<String>,
}

/// `blkid` exits with this when the device holds no recognisable signature.
const BLKID_NOTHING_FOUND: i32 = 2;

pub fn signature(runner: &dyn Runner, device: &Path) -> Result<Signature, RunError> {
    let command = CommandLine::new("blkid")
        .arg("--probe")
        .arg("--output")
        .arg("export")
        .path(device);
    let output = runner.run_expecting(&command, &[0, BLKID_NOTHING_FOUND])?;
    Ok(parse_signature(&output.text()))
}

pub fn parse_signature(text: &str) -> Signature {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    Signature {
        fstype: value("TYPE"),
        uuid: value("UUID"),
        label: value("LABEL"),
    }
}

/// The device node of partition `number` on `disk`: `/dev/sda3`, or `/dev/nvme0n1p3` when the
/// disk's name ends in a digit.
pub fn partition_path(disk: &Path, number: u32) -> PathBuf {
    let name = disk.as_os_str().to_string_lossy();
    let separator = if name.ends_with(|c: char| c.is_ascii_digit()) {
        "p"
    } else {
        ""
    };
    PathBuf::from(format!("{name}{separator}{number}"))
}

pub fn find_disk<'a>(disks: &'a [DiskInfo], path: &Path) -> Option<&'a DiskInfo> {
    disks.iter().find(|disk| disk.path == path)
}

pub fn parse_lsblk(json: &str) -> Result<Vec<DiskInfo>, String> {
    let output: LsblkOutput = serde_json::from_str(json).map_err(|e| e.to_string())?;
    output
        .blockdevices
        .into_iter()
        .filter(|device| device.kind == "disk")
        .map(disk_info)
        .collect()
}

fn disk_info(device: RawDevice) -> Result<DiskInfo, String> {
    let logical_sector_size = device
        .log_sec
        .ok_or_else(|| format!("{} has no logical sector size", device.path.display()))?;
    let partitions = device
        .children
        .into_iter()
        .filter(|child| child.kind == "part")
        .map(partition_info)
        .collect::<Result<_, _>>()?;
    Ok(DiskInfo {
        path: device.path,
        size: device.size,
        logical_sector_size,
        transport: device.tran,
        removable: device.rm || device.hotplug,
        partition_table: device.pttype,
        partitions,
    })
}

fn partition_info(device: RawDevice) -> Result<PartitionInfo, String> {
    let missing = |what: &str| format!("{} has no {what}", device.path.display());
    Ok(PartitionInfo {
        number: device.partn.ok_or_else(|| missing("partition number"))?,
        start: Lba(device.start.ok_or_else(|| missing("start sector"))?),
        size: device.size,
        unique_guid: parse_guid(device.partuuid.as_deref())?,
        type_guid: parse_guid(device.parttype.as_deref())?,
        mountpoints: device.mountpoints.into_iter().flatten().collect(),
        fstype: device.fstype,
        uuid: device.uuid,
        label: device.label,
        path: device.path,
    })
}

fn parse_guid(text: Option<&str>) -> Result<Option<Guid>, String> {
    text.map(|t| t.parse::<Guid>().map_err(|e| e.to_string()))
        .transpose()
}

#[derive(Deserialize)]
struct LsblkOutput {
    blockdevices: Vec<RawDevice>,
}

#[derive(Deserialize)]
struct RawDevice {
    path: PathBuf,
    #[serde(rename = "type")]
    kind: String,
    size: u64,
    #[serde(rename = "log-sec")]
    log_sec: Option<u32>,
    tran: Option<String>,
    #[serde(default)]
    rm: bool,
    #[serde(default)]
    hotplug: bool,
    pttype: Option<String>,
    fstype: Option<String>,
    uuid: Option<String>,
    label: Option<String>,
    partuuid: Option<String>,
    parttype: Option<String>,
    partn: Option<u32>,
    start: Option<u64>,
    #[serde(default)]
    mountpoints: Vec<Option<PathBuf>>,
    #[serde(default)]
    children: Vec<RawDevice>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUAL_BOOT: &str = include_str!("../tests/fixtures/lsblk/dual-boot.json");

    fn laptop() -> DiskInfo {
        parse_lsblk(DUAL_BOOT).unwrap().remove(0)
    }

    #[test]
    fn finds_only_disks_with_their_partitions() {
        let disks = parse_lsblk(DUAL_BOOT).unwrap();
        let paths: Vec<_> = disks.iter().map(|d| d.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["/dev/nvme0n1", "/dev/sda"]);
        assert_eq!(disks[0].partitions.len(), 6);
    }

    #[test]
    fn reads_partition_geometry_and_identity() {
        let windows = laptop().partition(3).cloned().unwrap();
        assert_eq!(windows.start, Lba(567_296));
        assert_eq!(windows.size, 214_748_364_800);
        assert_eq!(windows.label.as_deref(), Some("Windows"));
        assert_eq!(
            windows.unique_guid.map(|g| g.to_string()).as_deref(),
            Some("9c8b7a6d-5e4f-4321-a0b1-c2d3e4f5a6b7")
        );
    }

    #[test]
    fn maps_each_partition_to_what_bootshift_may_do() {
        let contents: Vec<Content> = laptop()
            .partitions
            .iter()
            .map(PartitionInfo::content)
            .collect();
        assert_eq!(
            contents,
            [
                Content::EfiSystem,
                Content::Opaque,
                Content::Ntfs,
                Content::Ntfs,
                Content::Ext4,
                Content::Swap,
            ]
        );
    }

    #[test]
    fn records_mounts_and_ignores_empty_mountpoints() {
        let disk = laptop();
        assert!(disk.partition(5).unwrap().is_mounted());
        assert!(!disk.partition(3).unwrap().is_mounted());
    }

    #[test]
    fn usb_disks_are_recognised() {
        let disks = parse_lsblk(DUAL_BOOT).unwrap();
        assert!(!disks[0].is_usb());
        assert!(disks[1].is_usb() && disks[1].removable);
    }

    #[test]
    fn partition_paths_follow_the_kernel_naming() {
        assert_eq!(
            partition_path(Path::new("/dev/sda"), 3),
            Path::new("/dev/sda3")
        );
        assert_eq!(
            partition_path(Path::new("/dev/nvme0n1"), 3),
            Path::new("/dev/nvme0n1p3")
        );
        assert_eq!(
            partition_path(Path::new("/dev/mmcblk0"), 1),
            Path::new("/dev/mmcblk0p1")
        );
    }

    #[test]
    fn hibernated_swap_is_only_ever_moved() {
        assert_eq!(content_of(Some("swsuspend"), None), Content::Opaque);
        assert_eq!(content_of(Some("BitLocker"), None), Content::BitLocker);
    }

    #[test]
    fn reads_a_blkid_signature() {
        let text = "DEVNAME=/dev/sda6\nLABEL=myswap\nUUID=c0ffee00-1234-4abc-8def-001122334455\nTYPE=swap\nUSAGE=other\n";
        assert_eq!(
            parse_signature(text),
            Signature {
                fstype: Some("swap".to_string()),
                uuid: Some("c0ffee00-1234-4abc-8def-001122334455".to_string()),
                label: Some("myswap".to_string()),
            }
        );
        assert_eq!(parse_signature(""), Signature::default());
    }

    #[test]
    fn a_partition_without_a_start_is_refused() {
        let broken = DUAL_BOOT.replacen("\"start\": 2048", "\"start\": null", 1);
        assert!(parse_lsblk(&broken).unwrap_err().contains("start sector"));
    }
}
