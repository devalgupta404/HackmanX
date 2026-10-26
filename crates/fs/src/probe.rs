//! Finding disks and partitions with `lsblk`.
//!
//! util-linux grew the columns and JSON shape BootShift prefers over time. Before 2.37, lsblk
//! has no PARTN, START or MOUNTPOINTS columns, prints partitions as a flat list instead of
//! nesting them under their disk, and (before 2.34) quotes numbers and booleans as strings.
//! `disks` first asks with the modern columns and falls back to the old set, deriving the
//! partition number from the device name and reading start sectors from `sfdisk --json`.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use bootshift_core::layout::{Content, Guid};
use bootshift_core::units::Lba;
use serde::{Deserialize, Deserializer};

use crate::parse::unexpected;
use crate::run::{CommandLine, RunError, Runner};

const MODERN_COLUMNS: &str = "PATH,TYPE,SIZE,LOG-SEC,TRAN,RM,HOTPLUG,PTTYPE,FSTYPE,UUID,LABEL,PARTUUID,PARTTYPE,PARTN,START,MOUNTPOINTS";
const COMPAT_COLUMNS: &str =
    "PATH,TYPE,SIZE,LOG-SEC,TRAN,RM,HOTPLUG,PTTYPE,FSTYPE,UUID,LABEL,PARTUUID,PARTTYPE,MOUNTPOINT";

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
    let command = lsblk(MODERN_COLUMNS);
    match runner.run_ok(&command) {
        Ok(output) => {
            let groups = grouped_devices(&output.text()).map_err(|d| unexpected(&command, d))?;
            build_disks(groups, |_, device| device.start)
                .map_err(|detail| unexpected(&command, detail))
        }
        Err(RunError::Failed { stderr, .. }) if stderr.contains("unknown column") => {
            compat_disks(runner)
        }
        Err(error) => Err(error),
    }
}

/// The old-lsblk path: no PARTN, START or MOUNTPOINTS columns, partitions listed flat.
/// Start sectors come from `sfdisk --json` instead.
fn compat_disks(runner: &dyn Runner) -> Result<Vec<DiskInfo>, RunError> {
    let command = lsblk(COMPAT_COLUMNS);
    let output = runner.run_ok(&command)?;
    let groups = grouped_devices(&output.text()).map_err(|d| unexpected(&command, d))?;
    let mut starts = BTreeMap::new();
    for (disk, partitions) in &groups {
        if !partitions.is_empty() {
            starts.extend(sfdisk_starts(runner, &disk.path)?);
        }
    }
    build_disks(groups, |path, device| {
        device.start.or_else(|| starts.get(path).copied())
    })
    .map_err(|detail| unexpected(&command, detail))
}

fn lsblk(columns: &str) -> CommandLine {
    CommandLine::new("lsblk")
        .arg("--json")
        .arg("--bytes")
        .arg("--output")
        .arg(columns)
}

fn sfdisk_starts(runner: &dyn Runner, disk: &Path) -> Result<BTreeMap<PathBuf, u64>, RunError> {
    let command = CommandLine::new("sfdisk").arg("--json").path(disk);
    let output = runner.run_ok(&command)?;
    let dump: SfdiskDump = serde_json::from_str(&output.text())
        .map_err(|e| unexpected(&command, e.to_string()))?;
    Ok(dump
        .partitiontable
        .partitions
        .into_iter()
        .map(|p| (p.node, p.start))
        .collect())
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

/// The partition number in a device name: 3 for `/dev/sda3` on `/dev/sda`, or for
/// `/dev/nvme0n1p3` on `/dev/nvme0n1`.
fn number_in_path(disk: &Path, partition: &Path) -> Option<u32> {
    let disk = disk.to_str()?;
    let suffix = partition.to_str()?.strip_prefix(disk)?;
    let digits = suffix.strip_prefix('p').unwrap_or(suffix);
    digits.parse().ok()
}

pub fn find_disk<'a>(disks: &'a [DiskInfo], path: &Path) -> Option<&'a DiskInfo> {
    disks.iter().find(|disk| disk.path == path)
}

pub fn parse_lsblk(json: &str) -> Result<Vec<DiskInfo>, String> {
    build_disks(grouped_devices(json)?, |_, device| device.start)
}

/// Disks paired with their partitions, whether lsblk nested the partitions as `children`
/// (2.37 and later) or listed every device flat (older).
fn grouped_devices(json: &str) -> Result<Vec<(RawDevice, Vec<RawDevice>)>, String> {
    let output: LsblkOutput = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut groups: Vec<(RawDevice, Vec<RawDevice>)> = Vec::new();
    let mut flat_parts: Vec<RawDevice> = Vec::new();
    for mut device in output.blockdevices {
        let children = std::mem::take(&mut device.children);
        match device.kind.as_str() {
            "disk" => {
                let parts = children.into_iter().filter(|c| c.kind == "part").collect();
                groups.push((device, parts));
            }
            "part" => flat_parts.push(device),
            _ => {}
        }
    }
    for part in flat_parts {
        let disk = groups
            .iter_mut()
            .find(|(disk, _)| number_in_path(&disk.path, &part.path).is_some())
            .ok_or_else(|| format!("{} belongs to no listed disk", part.path.display()))?;
        disk.1.push(part);
    }
    Ok(groups)
}

fn build_disks(
    groups: Vec<(RawDevice, Vec<RawDevice>)>,
    start_of: impl Fn(&Path, &RawDevice) -> Option<u64>,
) -> Result<Vec<DiskInfo>, String> {
    groups
        .into_iter()
        .map(|(device, parts)| {
            let logical_sector_size = device
                .log_sec
                .ok_or_else(|| format!("{} has no logical sector size", device.path.display()))?;
            let partitions = parts
                .into_iter()
                .map(|part| partition_info(&device.path, part, &start_of))
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
        })
        .collect()
}

fn partition_info(
    disk: &Path,
    device: RawDevice,
    start_of: &impl Fn(&Path, &RawDevice) -> Option<u64>,
) -> Result<PartitionInfo, String> {
    let missing = |what: &str| format!("{} has no {what}", device.path.display());
    let start = start_of(&device.path, &device).ok_or_else(|| missing("start sector"))?;
    let number = device
        .partn
        .or_else(|| number_in_path(disk, &device.path))
        .ok_or_else(|| missing("partition number"))?;
    let mut mountpoints: Vec<PathBuf> = device.mountpoints.into_iter().flatten().collect();
    mountpoints.extend(device.mountpoint);
    Ok(PartitionInfo {
        number,
        start: Lba(start),
        size: device.size,
        unique_guid: parse_guid(device.partuuid.as_deref())?,
        type_guid: parse_guid(device.parttype.as_deref())?,
        mountpoints,
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
    #[serde(deserialize_with = "number")]
    size: u64,
    #[serde(rename = "log-sec", default, deserialize_with = "optional_number")]
    log_sec: Option<u32>,
    tran: Option<String>,
    #[serde(default, deserialize_with = "flexible_bool")]
    rm: bool,
    #[serde(default, deserialize_with = "flexible_bool")]
    hotplug: bool,
    pttype: Option<String>,
    fstype: Option<String>,
    uuid: Option<String>,
    label: Option<String>,
    partuuid: Option<String>,
    parttype: Option<String>,
    #[serde(default, deserialize_with = "optional_number")]
    partn: Option<u32>,
    #[serde(default, deserialize_with = "optional_number")]
    start: Option<u64>,
    #[serde(default)]
    mountpoints: Vec<Option<PathBuf>>,
    /// The singular column of lsblk before 2.37.
    #[serde(default)]
    mountpoint: Option<PathBuf>,
    #[serde(default)]
    children: Vec<RawDevice>,
}

#[derive(Deserialize)]
struct SfdiskDump {
    partitiontable: SfdiskTable,
}

#[derive(Deserialize)]
struct SfdiskTable {
    #[serde(default)]
    partitions: Vec<SfdiskPartition>,
}

#[derive(Deserialize)]
struct SfdiskPartition {
    node: PathBuf,
    start: u64,
}

/// A JSON number, or the same number quoted as a string (lsblk before 2.34).
#[derive(Deserialize)]
#[serde(untagged)]
enum MaybeQuoted<T> {
    Plain(T),
    Quoted(String),
}

impl<T: FromStr> MaybeQuoted<T>
where
    T::Err: Display,
{
    fn value<E: serde::de::Error>(self) -> Result<T, E> {
        match self {
            Self::Plain(value) => Ok(value),
            Self::Quoted(text) => text.parse().map_err(E::custom),
        }
    }
}

fn number<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
    T::Err: Display,
{
    MaybeQuoted::deserialize(deserializer)?.value()
}

fn optional_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
    T::Err: Display,
{
    Option::<MaybeQuoted<T>>::deserialize(deserializer)?
        .map(MaybeQuoted::value)
        .transpose()
}

fn flexible_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawBool {
        Plain(bool),
        Text(String),
    }
    match RawBool::deserialize(deserializer)? {
        RawBool::Plain(value) => Ok(value),
        RawBool::Text(text) => match text.as_str() {
            "0" | "false" | "" => Ok(false),
            "1" | "true" => Ok(true),
            other => Err(serde::de::Error::custom(format!("not a boolean: {other}"))),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::run::ScriptedRunner;

    use super::*;

    const DUAL_BOOT: &str = include_str!("../tests/fixtures/lsblk/dual-boot.json");
    const OLD_FLAT: &str = include_str!("../tests/fixtures/lsblk/old-flat.json");
    const OLD_SFDISK: &str = include_str!("../tests/fixtures/lsblk/old-flat-sfdisk.json");

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
    fn partition_numbers_are_read_back_out_of_device_names() {
        assert_eq!(
            number_in_path(Path::new("/dev/sda"), Path::new("/dev/sda3")),
            Some(3)
        );
        assert_eq!(
            number_in_path(Path::new("/dev/nvme0n1"), Path::new("/dev/nvme0n1p12")),
            Some(12)
        );
        assert_eq!(
            number_in_path(Path::new("/dev/sda"), Path::new("/dev/sdb1")),
            None
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

    #[test]
    fn old_lsblk_without_the_new_columns_falls_back_to_sfdisk() {
        let runner = ScriptedRunner::new()
            .reply_error(1, "lsblk: unknown column: PARTN,START,MOUNTPOINTS")
            .reply(0, OLD_FLAT)
            .reply(0, OLD_SFDISK);
        let disks = disks(&runner).unwrap();

        let commands = runner.commands();
        assert_eq!(commands.len(), 3);
        assert!(commands[1].contains("MOUNTPOINT"));
        assert!(!commands[1].contains("MOUNTPOINTS"));
        assert_eq!(commands[2], "sfdisk --json /dev/sda");

        assert_eq!(disks.len(), 2);
        let sda = &disks[0];
        assert_eq!(sda.path, Path::new("/dev/sda"));
        let numbers: Vec<u32> = sda.partitions.iter().map(|p| p.number).collect();
        assert_eq!(numbers, [1, 2, 7]);
        assert_eq!(sda.partition(1).unwrap().start, Lba(2048));
        assert_eq!(sda.partition(2).unwrap().start, Lba(1_085_440));
        assert_eq!(sda.partition(7).unwrap().start, Lba(824_147_968));
        assert_eq!(sda.partition(2).unwrap().content(), Content::EfiSystem);
        assert!(sda.partition(7).unwrap().is_mounted());
        assert!(disks[1].partitions.is_empty());
    }

    #[test]
    fn quoted_numbers_and_booleans_still_parse() {
        let json = r#"{"blockdevices": [
            {"path":"/dev/sda", "type":"disk", "size":"1000204886016", "log-sec":"512",
             "tran":"sata", "rm":"0", "hotplug":"0", "pttype":"gpt", "fstype":null,
             "uuid":null, "label":null, "partuuid":null, "parttype":null,
             "children": [
                {"path":"/dev/sda1", "type":"part", "size":"554696704", "log-sec":"512",
                 "tran":null, "rm":"0", "hotplug":"1", "pttype":"gpt", "fstype":"ext4",
                 "uuid":null, "label":null, "partuuid":null, "parttype":null,
                 "partn":"1", "start":"2048", "mountpoint":"/"}
             ]}
        ]}"#;
        let disks = parse_lsblk(json).unwrap();
        assert_eq!(disks[0].size, 1_000_204_886_016);
        assert!(!disks[0].removable);
        let part = disks[0].partition(1).unwrap();
        assert_eq!(part.start, Lba(2048));
        assert!(part.is_mounted());
        assert!(disks[0].partitions[0].size == 554_696_704);
    }

    #[test]
    fn an_orphan_partition_is_refused() {
        let json = r#"{"blockdevices": [
            {"path":"/dev/sdb1", "type":"part", "size":1024, "log-sec":512,
             "tran":null, "rm":false, "hotplug":false, "pttype":null, "fstype":null,
             "uuid":null, "label":null, "partuuid":null, "parttype":null}
        ]}"#;
        assert!(parse_lsblk(json).unwrap_err().contains("no listed disk"));
    }
}
