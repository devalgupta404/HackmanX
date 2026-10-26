//! Mounting a filesystem for the operations the kernel does online (growing ext4, XFS and
//! Btrfs, shrinking Btrfs), which are journaled and so survive a power cut.

use std::path::Path;

use crate::run::{CommandLine, RunError, Runner};

/// Mounts `device` at `mountpoint`, runs `work`, and unmounts again even if `work` failed.
pub fn with_mounted<T>(
    runner: &dyn Runner,
    device: &Path,
    fstype: &str,
    mountpoint: &Path,
    work: impl FnOnce(&Path) -> Result<T, RunError>,
) -> Result<T, RunError> {
    std::fs::create_dir_all(mountpoint).map_err(|source| RunError::Start {
        command: format!("mkdir {}", mountpoint.display()),
        source,
    })?;
    runner.run_ok(
        &CommandLine::new("mount")
            .arg("-t")
            .arg(fstype)
            .path(device)
            .path(mountpoint),
    )?;
    let result = work(mountpoint);
    let unmounted = runner.run_ok(&CommandLine::new("umount").path(mountpoint));
    let value = result?;
    unmounted?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ScriptedRunner;

    #[test]
    fn unmounts_even_when_the_work_fails() {
        let mountpoint = tempfile::tempdir().unwrap();
        let runner = ScriptedRunner::new().reply(0, "").reply(0, "");

        let result: Result<(), RunError> = with_mounted(
            &runner,
            Path::new("/dev/sda3"),
            "ext4",
            mountpoint.path(),
            |_| {
                Err(RunError::UnexpectedOutput {
                    command: "work".to_string(),
                    detail: "failed".to_string(),
                })
            },
        );

        assert!(result.is_err());
        let commands = runner.commands();
        assert!(commands[0].starts_with("mount -t ext4 /dev/sda3"));
        assert!(commands[1].starts_with("umount"));
    }
}
