//! Swap partitions hold nothing worth copying, so they are recreated rather than moved.

use std::path::Path;

use crate::run::{CommandLine, RunError, Runner};

/// Writes a fresh swap signature with the old UUID and label, so `/etc/fstab` and the
/// `resume=` kernel option still find it. Only call this after checking the partition holds
/// no hibernation image (its signature type is `swap`, not `swsuspend`).
pub fn recreate(
    runner: &dyn Runner,
    device: &Path,
    uuid: &str,
    label: Option<&str>,
) -> Result<(), RunError> {
    let mut command = CommandLine::new("mkswap").arg("--uuid").arg(uuid);
    if let Some(label) = label {
        command = command.arg("--label").arg(label);
    }
    runner.run_ok(&command.path(device)).map(|_| ())
}
