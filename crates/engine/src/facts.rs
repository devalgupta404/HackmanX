//! Reading facts about the running computer for the preflight checks. Paths are relative to
//! a root, so tests can point them at a fake `/sys`.

use std::fs;
use std::path::Path;

use bootshift_core::layout::{Content, Guid, Layout};
use bootshift_core::refusal::Refusal;
use bootshift_fs::ntfs::DirtyMark;
use bootshift_fs::probe::DiskInfo;
use bootshift_fs::run::Runner;
use bootshift_fs::{ext4, ntfs};

use crate::preflight::Power;

/// Whether the system was started through UEFI.
pub fn booted_with_uefi(sys: &Path) -> bool {
    sys.join("firmware/efi").is_dir()
}

/// The kernel modules of the running kernel, which the recovery system copies, are present.
pub fn kernel_modules_present(lib_modules: &Path, kernel_release: &str) -> bool {
    lib_modules.join(kernel_release).is_dir()
}

/// Mains and battery state from `/sys/class/power_supply`. Any connected supply that isn't a
/// battery (mains, USB-C) counts as mains.
pub fn power(sys: &Path) -> Power {
    let mut on_mains = None;
    let mut battery_percent = None;
    let Ok(supplies) = fs::read_dir(sys.join("class/power_supply")) else {
        return Power {
            on_mains,
            battery_percent,
        };
    };
    for supply in supplies.flatten() {
        let path = supply.path();
        let read = |name: &str| fs::read_to_string(path.join(name)).ok();
        match read("type").as_deref().map(str::trim) {
            Some("Battery") => {
                let percent = read("capacity").and_then(|c| c.trim().parse::<u8>().ok());
                battery_percent = battery_percent.or(percent).or(Some(0));
            }
            Some(_) => {
                let online = read("online").map(|o| o.trim() == "1");
                on_mains = match (on_mains, online) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (_, Some(false)) => Some(false),
                    (current, None) => current,
                };
            }
            None => {}
        }
    }
    Power {
        on_mains,
        battery_percent,
    }
}

/// Free bytes on the filesystem mounted at `mountpoint`.
pub fn free_space(mountpoint: &Path) -> Option<u64> {
    let stats = rustix::fs::statvfs(mountpoint).ok()?;
    Some(stats.f_bavail * stats.f_frsize)
}

/// Problems with the filesystems the plan changes, found without modifying them.
pub fn filesystem_problems(
    runner: &dyn Runner,
    disk: &DiskInfo,
    layout: &Layout,
    changed: &[Guid],
) -> Vec<Refusal> {
    let mut problems = Vec::new();
    for partition in layout
        .partitions
        .iter()
        .filter(|p| changed.contains(&p.unique_guid))
    {
        let label = partition.label();
        let Some(device) = disk.partition(partition.number).map(|p| p.path.clone()) else {
            problems.push(Refusal::new(
                format!("The kernel doesn't list {label}."),
                "Restart the computer, then try again.",
            ));
            continue;
        };
        match partition.content {
            Content::Ext4 => match ext4::summary(runner, &device) {
                Ok(summary) if summary.clean && summary.error_count == 0 => {}
                Ok(_) => problems.push(Refusal::new(
                    format!("{label} has filesystem errors."),
                    "Check it with e2fsck (from a live USB if it is your running system), then try again.",
                )),
                Err(error) => problems.push(Refusal::new(
                    format!("BootShift couldn't read {label}: {error}"),
                    "Check it with e2fsck, then try again.",
                )),
            },
            Content::Ntfs => {
                if let Err(error) = ntfs::info(runner, &device, DirtyMark::Refuse) {
                    problems.push(Refusal::new(
                        format!("{label} isn't safe to change: {error}"),
                        "Start Windows, turn off Fast Startup, shut Windows down fully, then try again.",
                    ));
                }
            }
            _ => {}
        }
    }
    problems
}

/// A swap partition that holds a hibernated system.
pub fn has_hibernated_swap(disk: &DiskInfo) -> bool {
    disk.partitions
        .iter()
        .any(|p| p.fstype.as_deref() == Some("swsuspend"))
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::{Extent, Geometry, Partition};
    use bootshift_core::units::{Lba, Sectors};
    use bootshift_fs::probe::PartitionInfo;
    use bootshift_fs::run::ScriptedRunner;

    use super::*;

    const TUNE2FS_CLEAN: &str = "Filesystem state:         clean\nBlock count:              100\nBlock size:               4096\nFree blocks:              50\n";

    fn partition(number: u32, content: Content) -> Partition {
        Partition {
            number,
            unique_guid: Guid([number as u8; 16]),
            type_guid: Guid([0; 16]),
            name: String::new(),
            attributes: 0,
            extent: Extent::new(Lba(2048 * u64::from(number)), Sectors(2048)),
            content,
            used: Sectors(0),
            min_len: None,
        }
    }

    fn disk_and_layout(contents: &[Content]) -> (DiskInfo, Layout) {
        let partitions: Vec<Partition> = contents
            .iter()
            .enumerate()
            .map(|(i, &content)| partition(i as u32 + 1, content))
            .collect();
        let infos = partitions
            .iter()
            .map(|p| PartitionInfo {
                path: format!("/dev/sda{}", p.number).into(),
                number: p.number,
                start: p.extent.start,
                size: 1 << 20,
                fstype: None,
                uuid: None,
                label: None,
                unique_guid: Some(p.unique_guid),
                type_guid: None,
                mountpoints: Vec::new(),
            })
            .collect();
        let disk = DiskInfo {
            path: "/dev/sda".into(),
            size: 1 << 30,
            logical_sector_size: 512,
            transport: Some("sata".to_string()),
            removable: false,
            partition_table: Some("gpt".to_string()),
            partitions: infos,
        };
        let layout = Layout {
            geometry: Geometry {
                sector_size: 512,
                usable: Extent::new(Lba(34), Sectors(1 << 21)),
                alignment: Sectors(2048),
            },
            partitions,
        };
        (disk, layout)
    }

    #[test]
    fn clean_filesystems_have_no_problems() {
        let (disk, layout) = disk_and_layout(&[Content::Ext4]);
        let runner = ScriptedRunner::new().reply(0, TUNE2FS_CLEAN);

        assert!(filesystem_problems(&runner, &disk, &layout, &[Guid([1; 16])]).is_empty());
        assert_eq!(runner.commands(), ["tune2fs -l /dev/sda1"]);
    }

    #[test]
    fn a_hibernated_windows_volume_is_a_problem_with_a_fix() {
        let (disk, layout) = disk_and_layout(&[Content::Ntfs]);
        let runner = ScriptedRunner::new().reply_error(1, "The NTFS partition is hibernated.");

        let problems = filesystem_problems(&runner, &disk, &layout, &[Guid([1; 16])]);

        assert_eq!(problems.len(), 1);
        assert!(problems[0].reason.contains("hibernated"));
        assert!(problems[0].fix.contains("Fast Startup"));
    }

    #[test]
    fn unchanged_partitions_are_not_examined() {
        let (disk, layout) = disk_and_layout(&[Content::Ext4, Content::Ntfs]);
        let runner = ScriptedRunner::new();

        assert!(filesystem_problems(&runner, &disk, &layout, &[]).is_empty());
        assert!(runner.commands().is_empty());
    }

    #[test]
    fn an_ext4_with_errors_is_a_problem() {
        let (disk, layout) = disk_and_layout(&[Content::Ext4]);
        let dirty = TUNE2FS_CLEAN.replace("clean", "not clean");
        let runner = ScriptedRunner::new().reply(0, &dirty);

        let problems = filesystem_problems(&runner, &disk, &layout, &[Guid([1; 16])]);

        assert!(problems[0].reason.contains("filesystem errors"));
    }

    fn supply(sys: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = sys.join("class/power_supply").join(name);
        fs::create_dir_all(&dir).unwrap();
        for (file, contents) in files {
            fs::write(dir.join(file), contents).unwrap();
        }
    }

    #[test]
    fn reads_a_laptop_on_its_charger() {
        let sys = tempfile::tempdir().unwrap();
        supply(sys.path(), "AC", &[("type", "Mains\n"), ("online", "1\n")]);
        supply(
            sys.path(),
            "BAT0",
            &[("type", "Battery\n"), ("capacity", "73\n")],
        );

        assert_eq!(
            power(sys.path()),
            Power {
                on_mains: Some(true),
                battery_percent: Some(73),
            }
        );
    }

    #[test]
    fn a_usb_c_charger_counts_as_mains() {
        let sys = tempfile::tempdir().unwrap();
        supply(sys.path(), "AC", &[("type", "Mains\n"), ("online", "0\n")]);
        supply(
            sys.path(),
            "ucsi-source-psy-1",
            &[("type", "USB\n"), ("online", "1\n")],
        );
        supply(
            sys.path(),
            "BAT0",
            &[("type", "Battery\n"), ("capacity", "90\n")],
        );

        assert_eq!(power(sys.path()).on_mains, Some(true));
    }

    #[test]
    fn a_battery_without_a_readable_level_counts_as_empty() {
        let sys = tempfile::tempdir().unwrap();
        supply(sys.path(), "BAT0", &[("type", "Battery\n")]);

        assert_eq!(power(sys.path()).battery_percent, Some(0));
    }

    #[test]
    fn a_desktop_has_no_battery() {
        let sys = tempfile::tempdir().unwrap();
        assert_eq!(
            power(sys.path()),
            Power {
                on_mains: None,
                battery_percent: None,
            }
        );
    }

    #[test]
    fn detects_uefi_and_kernel_modules() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("firmware/efi")).unwrap();
        fs::create_dir_all(root.path().join("6.8.0-45-generic")).unwrap();

        assert!(booted_with_uefi(root.path()));
        assert!(kernel_modules_present(root.path(), "6.8.0-45-generic"));
        assert!(!kernel_modules_present(root.path(), "6.9.0-1-generic"));
    }

    #[test]
    fn free_space_of_a_real_directory_is_readable() {
        assert!(free_space(Path::new(env!("CARGO_MANIFEST_DIR"))).is_some());
    }
}
