//! Btrfs through btrfs-progs. Resizing is done online by the kernel, which relocates data
//! transactionally, so it survives a power cut without a guarded run.

use std::path::Path;

use crate::run::{CommandLine, RunError, Runner};

/// Resizes the mounted filesystem at `mountpoint` to `size` bytes, or to fill its partition.
pub fn resize(runner: &dyn Runner, mountpoint: &Path, size: Option<u64>) -> Result<(), RunError> {
    let size = size.map_or_else(|| "max".to_string(), |bytes| bytes.to_string());
    let command = CommandLine::new("btrfs")
        .arg("filesystem")
        .arg("resize")
        .arg(size)
        .path(mountpoint);
    runner.run_ok(&command).map(|_| ())
}

/// A read-only check of an unmounted filesystem.
pub fn check(runner: &dyn Runner, device: &Path) -> Result<(), RunError> {
    let command = CommandLine::new("btrfs")
        .arg("check")
        .arg("--readonly")
        .path(device);
    runner.run_ok(&command).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ScriptedRunner;

    #[test]
    fn grows_to_max_and_shrinks_to_a_size() {
        let runner = ScriptedRunner::new().reply(0, "").reply(0, "");
        resize(&runner, Path::new("/mnt"), None).unwrap();
        resize(&runner, Path::new("/mnt"), Some(1 << 30)).unwrap();
        assert_eq!(
            runner.commands(),
            [
                "btrfs filesystem resize max /mnt",
                "btrfs filesystem resize 1073741824 /mnt"
            ]
        );
    }
}
