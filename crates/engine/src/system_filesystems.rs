//! The real filesystem tools, run on partition device nodes in the recovery system.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bootshift_core::layout::{Content, Guid, Partition};
use bootshift_core::scope::CopyScope;
use bootshift_fs::ntfs::DirtyMark;
use bootshift_fs::probe::partition_path;
use bootshift_fs::run::{RunError, Runner};
use bootshift_fs::{btrfs, ext4, kernel, mount, ntfs, swap, xfs};

use crate::executor::Filesystems;
use crate::run_plan::SwapIdentity;

pub struct SystemFilesystems<'a> {
    runner: &'a dyn Runner,
    disk: PathBuf,
    sector_size: u32,
    mountpoint: PathBuf,
    swaps: BTreeMap<Guid, SwapIdentity>,
}

impl<'a> SystemFilesystems<'a> {
    pub fn new(
        runner: &'a dyn Runner,
        disk: &Path,
        sector_size: u32,
        mountpoint: &Path,
        swaps: BTreeMap<Guid, SwapIdentity>,
    ) -> Self {
        Self {
            runner,
            disk: disk.to_path_buf(),
            sector_size,
            mountpoint: mountpoint.to_path_buf(),
            swaps,
        }
    }

    fn device(&self, partition: &Partition) -> PathBuf {
        partition_path(&self.disk, partition.number)
    }

    fn len_bytes(&self, partition: &Partition) -> u64 {
        partition.extent.len.to_bytes(self.sector_size)
    }

    fn mounted<T>(
        &self,
        partition: &Partition,
        fstype: &str,
        work: impl FnOnce(&Path) -> Result<T, RunError>,
    ) -> Result<T, RunError> {
        mount::with_mounted(
            self.runner,
            &self.device(partition),
            fstype,
            &self.mountpoint,
            work,
        )
    }
}

impl Filesystems for SystemFilesystems<'_> {
    fn used_ranges(
        &mut self,
        partition: &Partition,
        resized_by_us: bool,
    ) -> Result<CopyScope, RunError> {
        let device = self.device(partition);
        let len = self.len_bytes(partition);
        match partition.content {
            Content::Ext4 => {
                ext4::check(self.runner, &device)?;
                Ok(CopyScope::used_only(
                    len,
                    ext4::used_ranges(self.runner, &device)?,
                ))
            }
            Content::Ntfs => {
                let dirty = dirty_mark(resized_by_us);
                let info = ntfs::info(self.runner, &device, dirty)?;
                let used = ntfs::used_ranges(self.runner, &device, &info, dirty)?;
                Ok(CopyScope::used_only(len, used))
            }
            _ => Ok(CopyScope::Everything),
        }
    }

    fn shrink(&mut self, partition: &Partition, len: u64) -> Result<(), RunError> {
        match partition.content {
            Content::Btrfs => self.mounted(partition, "btrfs", |mountpoint| {
                btrfs::resize(self.runner, mountpoint, Some(len))
            }),
            _ => Err(needs_guarded_run("shrink", partition)),
        }
    }

    fn grow(&mut self, partition: &Partition) -> Result<(), RunError> {
        let device = self.device(partition);
        match partition.content {
            Content::Ext4 => self.mounted(partition, "ext4", |_| {
                ext4::grow_online(self.runner, &device)
            }),
            Content::Btrfs => self.mounted(partition, "btrfs", |mountpoint| {
                btrfs::resize(self.runner, mountpoint, None)
            }),
            Content::Xfs => self.mounted(partition, "xfs", |mountpoint| {
                xfs::grow(self.runner, mountpoint)
            }),
            _ => Err(needs_guarded_run("grow", partition)),
        }
    }

    fn recreate_swap(&mut self, partition: &Partition) -> Result<(), RunError> {
        let identity =
            self.swaps
                .get(&partition.unique_guid)
                .ok_or_else(|| RunError::UnexpectedOutput {
                    command: "recreate swap".to_string(),
                    detail: format!("no recorded UUID for {}", partition.label()),
                })?;
        swap::recreate(
            self.runner,
            &self.device(partition),
            &identity.uuid,
            identity.label.as_deref(),
        )
    }

    fn check(&mut self, partition: &Partition, resized_by_us: bool) -> Result<(), RunError> {
        let device = self.device(partition);
        match partition.content {
            Content::Ext4 => ext4::check(self.runner, &device),
            Content::Ntfs => {
                ntfs::info(self.runner, &device, dirty_mark(resized_by_us)).map(|_| ())
            }
            Content::Btrfs => btrfs::check(self.runner, &device),
            Content::Xfs => xfs::check(self.runner, &device),
            _ => Ok(()),
        }
    }

    fn refresh_kernel_view(&mut self) -> Result<(), RunError> {
        kernel::refresh_partitions(self.runner, &self.disk)
    }
}

fn dirty_mark(resized_by_us: bool) -> DirtyMark {
    if resized_by_us {
        DirtyMark::ExpectedAfterOurResize
    } else {
        DirtyMark::Refuse
    }
}

fn needs_guarded_run(action: &str, partition: &Partition) -> RunError {
    RunError::UnexpectedOutput {
        command: format!("{action} {}", partition.label()),
        detail: "this needs a guarded run, which this build doesn't have yet".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::{Extent, Guid};
    use bootshift_core::units::{Lba, Sectors};
    use bootshift_fs::run::ScriptedRunner;

    use super::*;

    fn partition(number: u32, content: Content) -> Partition {
        Partition {
            number,
            unique_guid: Guid([number as u8; 16]),
            type_guid: Guid([0; 16]),
            name: String::new(),
            attributes: 0,
            extent: Extent::new(Lba(2048), Sectors(4096)),
            content,
            used: Sectors(0),
            min_len: None,
        }
    }

    fn filesystems<'a>(runner: &'a ScriptedRunner, mountpoint: &Path) -> SystemFilesystems<'a> {
        let swaps = BTreeMap::from([(
            Guid([4; 16]),
            SwapIdentity {
                uuid: "c0ffee00-1234-4abc-8def-001122334455".to_string(),
                label: None,
            },
        )]);
        SystemFilesystems::new(runner, Path::new("/dev/nvme0n1"), 512, mountpoint, swaps)
    }

    #[test]
    fn grows_ext4_online_while_mounted() {
        let mountpoint = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new().reply(0, "").reply(0, "").reply(0, "");

        filesystems(&runner, mountpoint.path())
            .grow(&partition(3, Content::Ext4))
            .unwrap();

        let commands = runner.commands();
        assert!(commands[0].starts_with("mount -t ext4 /dev/nvme0n1p3"));
        assert_eq!(commands[1], "resize2fs /dev/nvme0n1p3");
        assert!(commands[2].starts_with("umount"));
    }

    #[test]
    fn refuses_to_shrink_ext4_or_ntfs_without_a_guarded_run() {
        let mountpoint = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new();
        let mut filesystems = filesystems(&runner, mountpoint.path());

        assert!(
            filesystems
                .shrink(&partition(3, Content::Ext4), 1 << 20)
                .is_err()
        );
        assert!(
            filesystems
                .shrink(&partition(2, Content::Ntfs), 1 << 20)
                .is_err()
        );
        assert!(filesystems.grow(&partition(2, Content::Ntfs)).is_err());
        assert!(runner.commands().is_empty());
    }

    #[test]
    fn recreates_swap_with_its_recorded_uuid() {
        let mountpoint = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new().reply(0, "");

        filesystems(&runner, mountpoint.path())
            .recreate_swap(&partition(4, Content::Swap))
            .unwrap();

        assert_eq!(
            runner.commands(),
            ["mkswap --uuid c0ffee00-1234-4abc-8def-001122334455 /dev/nvme0n1p4"]
        );
    }

    #[test]
    fn used_ranges_of_unknown_contents_are_everything() {
        let mountpoint = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new();
        let scope = filesystems(&runner, mountpoint.path())
            .used_ranges(&partition(5, Content::Opaque), false)
            .unwrap();
        assert_eq!(scope, CopyScope::Everything);
    }
}
