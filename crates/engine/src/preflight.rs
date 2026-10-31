//! The checks that must all pass before Kitten changes a single byte. Anything that can't
//! be determined counts as a refusal.

use bootshift_core::layout::{Content, Layout};
use bootshift_core::plan::{Operation, Plan};
use bootshift_core::refusal::Refusal;
use bootshift_fs::smart::Health;

const MIN_BATTERY_PERCENT: u8 = 50;

/// What the normal system found out about the computer and the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub uefi: bool,
    pub partition_table: Option<String>,
    pub usb_disk: bool,
    pub power: Power,
    pub health: Health,
    pub kernel_modules_present: bool,
    pub esp: EspSpace,
    pub bootloader: Bootloader,
    pub filesystem_problems: Vec<Refusal>,
    pub hibernated_swap: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Power {
    /// Whether a mains or USB-C supply is connected; `None` if that couldn't be read.
    pub on_mains: Option<bool>,
    /// `None` on computers without a battery.
    pub battery_percent: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EspSpace {
    pub free: Option<u64>,
    /// `None` when the running kernel's image couldn't be found to measure.
    pub needed: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bootloader {
    Grub { vendor: String },
    SystemdBoot,
    Unsupported(String),
}

/// Every reason to refuse, so the user can fix them all at once. Empty means safe to start.
pub fn evaluate(facts: &Facts, layout: &Layout) -> Vec<Refusal> {
    let mut refusals = Vec::new();
    let mut refuse = |reason: &str, fix: &str| refusals.push(Refusal::new(reason, fix));

    if !facts.uefi {
        refuse(
            "This computer starts in legacy BIOS mode.",
            "Kitten needs UEFI. Switch the firmware to UEFI mode, which usually means reinstalling the operating systems.",
        );
    }
    if facts.partition_table.as_deref() != Some("gpt") {
        refuse(
            "The disk doesn't use a GPT partition table.",
            "Back up the disk and convert it to GPT (for example with gdisk), then try again.",
        );
    }
    if facts.usb_disk {
        refuse(
            "The disk is connected over USB, and USB adapters often ignore the commands that make writes durable.",
            "Connect the disk directly to the computer.",
        );
    }
    check_power(&facts.power, &mut refuse);
    match &facts.health {
        Health::Passed => {}
        Health::Failing => refuse(
            "The disk reports that it is failing.",
            "Back up your data and replace the disk. Don't rearrange partitions on a failing disk.",
        ),
        Health::Unknown(why) => refuse(
            &format!("Kitten couldn't read the disk's health ({why})."),
            "Install smartmontools, then try again.",
        ),
    }
    if !facts.kernel_modules_present {
        refuse(
            "The running kernel's modules are missing, probably because the kernel was updated.",
            "Restart the computer, then try again.",
        );
    }
    match (facts.esp.free, facts.esp.needed) {
        (Some(free), Some(needed)) if free >= needed => {}
        (Some(free), Some(needed)) => refuse(
            &format!(
                "The EFI system partition has {} MiB free, and Kitten needs {} MiB for its recovery system.",
                free >> 20,
                needed.div_ceil(1 << 20)
            ),
            "Remove old kernels or vendor files from the EFI system partition.",
        ),
        (None, _) => refuse(
            "Kitten couldn't find how much space is free on the EFI system partition.",
            "Make sure the EFI system partition is mounted (usually at /boot/efi).",
        ),
        (_, None) => refuse(
            "Kitten couldn't find the running kernel's image in /boot.",
            "Make sure /boot is mounted, restart if the kernel was just updated, then try again.",
        ),
    }
    if let Bootloader::Unsupported(why) = &facts.bootloader {
        refuse(
            &format!("Kitten can't start its recovery system with this bootloader: {why}."),
            "Use GRUB, or systemd-boot with Secure Boot off.",
        );
    }
    if facts.hibernated_swap {
        refuse(
            "A swap partition holds a hibernated system.",
            "Start that system, shut it down fully, then try again.",
        );
    }
    for partition in layout
        .partitions
        .iter()
        .filter(|p| p.content == Content::BitLocker)
    {
        refuse(
            &format!("{} is encrypted with BitLocker.", partition.label()),
            "In Windows, back up your recovery key, turn off device encryption, then try again.",
        );
    }
    refusals.extend(facts.filesystem_problems.iter().cloned());
    refusals
}

/// Operations this build can't yet do safely. Shrinking ext4 or NTFS, and growing NTFS, need a
/// guarded run (task 0.6): the tools that do them can't survive a power cut on their own.
pub fn unavailable_operations(layout: &Layout, plan: &Plan) -> Vec<Refusal> {
    let mut refusals = Vec::new();
    for operation in &plan.operations {
        let (partition, action) = match operation {
            Operation::Shrink { partition, .. } => (*partition, "shrink"),
            Operation::Grow { partition, .. } => (*partition, "grow"),
            _ => continue,
        };
        let Some(partition) = layout.partition(partition) else {
            continue;
        };
        let needs_guard = matches!(
            (action, partition.content),
            ("shrink", Content::Ext4 | Content::Ntfs) | ("grow", Content::Ntfs)
        );
        if needs_guard {
            refusals.push(Refusal::new(
                format!(
                    "This version of Kitten can't yet {action} {} safely.",
                    partition.label()
                ),
                format!("Keep {} at its current size for now.", partition.label()),
            ));
        }
    }
    refusals
}

fn check_power(power: &Power, refuse: &mut impl FnMut(&str, &str)) {
    let Some(percent) = power.battery_percent else {
        return;
    };
    match power.on_mains {
        Some(true) => {}
        Some(false) => refuse("The laptop is running on battery.", "Plug in the charger."),
        None => refuse(
            "Kitten couldn't tell whether the charger is connected.",
            "Plug in the charger and make sure it is detected.",
        ),
    }
    if percent < MIN_BATTERY_PERCENT {
        refuse(
            &format!("The battery is at {percent}%."),
            &format!(
                "Charge it to at least {MIN_BATTERY_PERCENT}% first, in case the charger is unplugged during the change."
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::{Extent, Geometry, Guid, Partition};
    use bootshift_core::units::{Lba, Sectors};

    use super::*;

    fn good_facts() -> Facts {
        Facts {
            uefi: true,
            partition_table: Some("gpt".to_string()),
            usb_disk: false,
            power: Power {
                on_mains: Some(true),
                battery_percent: Some(80),
            },
            health: Health::Passed,
            kernel_modules_present: true,
            esp: EspSpace {
                free: Some(80 << 20),
                needed: Some(40 << 20),
            },
            bootloader: Bootloader::Grub {
                vendor: "ubuntu".to_string(),
            },
            filesystem_problems: Vec::new(),
            hibernated_swap: false,
        }
    }

    fn layout_with(content: Content) -> Layout {
        Layout {
            geometry: Geometry {
                sector_size: 512,
                usable: Extent::new(Lba(34), Sectors(1 << 20)),
                alignment: Sectors(2048),
            },
            partitions: vec![Partition {
                number: 1,
                unique_guid: Guid([1; 16]),
                type_guid: Guid([2; 16]),
                name: String::new(),
                attributes: 0,
                extent: Extent::new(Lba(2048), Sectors(2048)),
                content,
                used: Sectors(0),
                min_len: None,
            }],
        }
    }

    fn reasons(facts: &Facts) -> Vec<String> {
        evaluate(facts, &layout_with(Content::Ext4))
            .into_iter()
            .map(|r| r.reason)
            .collect()
    }

    #[test]
    fn a_healthy_setup_passes() {
        assert!(reasons(&good_facts()).is_empty());
    }

    #[test]
    fn a_desktop_without_a_battery_passes() {
        let facts = Facts {
            power: Power {
                on_mains: None,
                battery_percent: None,
            },
            ..good_facts()
        };
        assert!(reasons(&facts).is_empty());
    }

    #[test]
    fn refuses_battery_power_and_a_low_battery() {
        let facts = Facts {
            power: Power {
                on_mains: Some(false),
                battery_percent: Some(30),
            },
            ..good_facts()
        };
        let reasons = reasons(&facts);
        assert_eq!(reasons.len(), 2);
        assert!(reasons[0].contains("battery") && reasons[1].contains("30%"));
    }

    #[test]
    fn unknown_disk_health_is_refused() {
        let facts = Facts {
            health: Health::Unknown("smartctl not found".to_string()),
            ..good_facts()
        };
        assert!(reasons(&facts)[0].contains("smartctl not found"));
    }

    #[test]
    fn unknown_esp_space_is_refused() {
        let facts = Facts {
            esp: EspSpace {
                free: None,
                needed: Some(1),
            },
            ..good_facts()
        };
        assert_eq!(reasons(&facts).len(), 1);
    }

    #[test]
    fn a_missing_kernel_image_is_refused() {
        let facts = Facts {
            esp: EspSpace {
                free: Some(80 << 20),
                needed: None,
            },
            ..good_facts()
        };
        assert!(reasons(&facts)[0].contains("kernel's image"));
    }

    #[test]
    fn too_little_esp_space_names_both_sizes() {
        let facts = Facts {
            esp: EspSpace {
                free: Some(20 << 20),
                needed: Some(40 << 20),
            },
            ..good_facts()
        };
        assert!(
            reasons(&facts)[0].contains("20 MiB free") && reasons(&facts)[0].contains("40 MiB")
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let facts = Facts {
            uefi: false,
            partition_table: Some("dos".to_string()),
            usb_disk: true,
            health: Health::Failing,
            kernel_modules_present: false,
            bootloader: Bootloader::Unsupported(
                "unified kernel images under Secure Boot".to_string(),
            ),
            hibernated_swap: true,
            filesystem_problems: vec![Refusal::new("ext4 has errors", "run e2fsck")],
            ..good_facts()
        };
        assert_eq!(reasons(&facts).len(), 8);
    }

    fn unavailable(content: Content, operation: Operation) -> usize {
        let plan = Plan {
            operations: vec![operation],
        };
        unavailable_operations(&layout_with(content), &plan).len()
    }

    #[test]
    fn shrinking_ext4_or_ntfs_and_growing_ntfs_wait_for_guarded_runs() {
        let shrink = Operation::Shrink {
            partition: Guid([1; 16]),
            len: Sectors(1024),
        };
        let grow = Operation::Grow {
            partition: Guid([1; 16]),
            len: Sectors(4096),
        };
        assert_eq!(unavailable(Content::Ext4, shrink.clone()), 1);
        assert_eq!(unavailable(Content::Ntfs, shrink.clone()), 1);
        assert_eq!(unavailable(Content::Ntfs, grow.clone()), 1);
        assert_eq!(unavailable(Content::Ext4, grow), 0);
        assert_eq!(unavailable(Content::Btrfs, shrink), 0);
    }

    #[test]
    fn a_bitlocker_volume_anywhere_on_the_disk_is_refused() {
        let refusals = evaluate(&good_facts(), &layout_with(Content::BitLocker));
        assert_eq!(refusals.len(), 1);
        assert!(refusals[0].fix.contains("turn off device encryption"));
    }
}
