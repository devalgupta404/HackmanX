//! Telling the kernel about a rewritten partition table.

use std::path::Path;

use crate::run::{CommandLine, RunError, Runner};

/// Updates the kernel's partition list for `disk` from the table on disk. `partx` changes one
/// partition at a time with BLKPG, which works while other partitions are in use, unlike
/// rereading the whole table (BLKRRPART).
pub fn refresh_partitions(runner: &dyn Runner, disk: &Path) -> Result<(), RunError> {
    runner
        .run_ok(&CommandLine::new("partx").arg("--update").path(disk))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ScriptedRunner;

    #[test]
    fn refreshes_with_partx_update() {
        let runner = ScriptedRunner::new().reply(0, "");
        refresh_partitions(&runner, Path::new("/dev/nvme0n1")).unwrap();
        assert_eq!(runner.commands(), ["partx --update /dev/nvme0n1"]);
    }
}
