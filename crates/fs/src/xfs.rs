//! XFS through xfsprogs. XFS can grow (online, journaled by the kernel) but never shrink.

use std::path::Path;

use crate::run::{CommandLine, RunError, Runner};

/// Grows the mounted filesystem at `mountpoint` to fill its partition.
pub fn grow(runner: &dyn Runner, mountpoint: &Path) -> Result<(), RunError> {
    runner
        .run_ok(&CommandLine::new("xfs_growfs").path(mountpoint))
        .map(|_| ())
}

/// A check that changes nothing, on an unmounted filesystem.
pub fn check(runner: &dyn Runner, device: &Path) -> Result<(), RunError> {
    runner
        .run_ok(&CommandLine::new("xfs_repair").arg("-n").path(device))
        .map(|_| ())
}
