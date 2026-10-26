//! What the recovery system does once it has found its disk: run the plan, then put normal
//! booting back.

use std::path::Path;

use bootshift_boot::cmdline::RecoveryArgs;
use bootshift_boot::{efi, grub};
use bootshift_core::journal::{Progress, Stage};
use bootshift_core::layout::Guid;
use bootshift_disk::{
    Access, BlockDevice, GptImage, Journal, JournalError, JournalFile, RealDevice,
};
use bootshift_fs::mount;
use bootshift_fs::probe::partition_path;
use bootshift_fs::run::{RunError, Runner};

use crate::copier::CopyOptions;
use crate::executor::{ExecError, execute};
use crate::run_plan::{BootRestore, RunPlan};
use crate::system_filesystems::SystemFilesystems;

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Disk(#[from] bootshift_disk::DiskError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("the journal's plan can't be read: {0}")]
    Plan(#[from] serde_json::Error),
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error(transparent)]
    Tool(#[from] RunError),
}

/// Whether `device` holds the GPT with this disk GUID.
pub fn disk_matches<D: BlockDevice + ?Sized>(device: &D, guid: Guid) -> bool {
    GptImage::read(device)
        .and_then(|table| table.disk_guid())
        .is_ok_and(|found| found == guid)
}

/// Runs the plan recorded in the journal to the end, then restores normal booting.
pub fn run(
    runner: &dyn Runner,
    disk_path: &Path,
    args: &RecoveryArgs,
    work_dir: &Path,
) -> Result<(), RecoveryError> {
    let mut disk = RealDevice::open(disk_path, Access::ReadWriteShared)?;
    let opened = Journal::open(&disk, JournalFile::new(args.journal.clone())?)?;
    let run = RunPlan::from_record(&opened.plan)?;
    let mut journal = opened.journal;

    let mut filesystems = SystemFilesystems::new(
        runner,
        disk_path,
        run.layout.geometry.sector_size,
        &work_dir.join("partition"),
        run.swaps.clone(),
    );
    execute(
        &mut disk,
        &mut journal,
        &mut filesystems,
        &run,
        opened.progress.clone(),
        &CopyOptions::default(),
    )?;

    let already_done = opened
        .progress
        .as_ref()
        .is_some_and(|p| p.stage == Stage::Done);
    if !already_done {
        restore_booting(runner, disk_path, &run.boot, &work_dir.join("esp"))?;
        journal.commit(&mut disk, &done())?;
    }
    Ok(())
}

/// Puts the original boot order and GRUB config back. Safe to repeat.
pub fn restore_booting(
    runner: &dyn Runner,
    disk_path: &Path,
    boot: &BootRestore,
    esp_mountpoint: &Path,
) -> Result<(), RunError> {
    efi::set_order(runner, &boot.original_boot_order)?;
    let esp = partition_path(disk_path, boot.esp_partition);
    mount::with_mounted(runner, &esp, "vfat", esp_mountpoint, |esp| {
        grub::remove_redirect(esp, &boot.grub_vendor).map_err(|source| RunError::Start {
            command: "restore grub.cfg".to_string(),
            source,
        })
    })
}

fn done() -> Progress {
    Progress {
        stage: Stage::Done,
        operation: 0,
        steps_done: 0,
        next_chunk: 0,
        hash_chain: [0; 32],
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use bootshift_disk::MemDisk;
    use bootshift_fs::run::ScriptedRunner;
    use gptman::GPT;

    use super::*;

    fn disk_with_guid(guid: [u8; 16]) -> MemDisk {
        let mut cursor = Cursor::new(vec![0; 1 << 20]);
        let mut gpt = GPT::new_from(&mut cursor, 512, guid).unwrap();
        gpt.write_into(&mut cursor).unwrap();
        MemDisk::from_bytes(cursor.into_inner(), 512).unwrap()
    }

    #[test]
    fn finds_the_disk_by_its_gpt_guid() {
        let disk = disk_with_guid([0xAB; 16]);
        assert!(disk_matches(&disk, Guid([0xAB; 16])));
        assert!(!disk_matches(&disk, Guid([0xCD; 16])));
        assert!(!disk_matches(
            &MemDisk::zeroed(1 << 20, 512).unwrap(),
            Guid([0xAB; 16])
        ));
    }

    #[test]
    fn restoring_puts_the_boot_order_back_and_then_the_grub_config() {
        let esp = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new().reply(0, "").reply(0, "").reply(0, "");
        let boot = BootRestore {
            esp_partition: 1,
            grub_vendor: "ubuntu".to_string(),
            original_boot_order: vec![0x0000, 0x0001],
        };

        restore_booting(&runner, Path::new("/dev/nvme0n1"), &boot, esp.path()).unwrap();

        let commands = runner.commands();
        assert_eq!(commands[0], "efibootmgr --bootorder 0000,0001");
        assert!(commands[1].starts_with("mount -t vfat /dev/nvme0n1p1"));
        assert!(commands[2].starts_with("umount"));
    }
}
