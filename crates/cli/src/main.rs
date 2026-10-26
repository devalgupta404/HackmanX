mod commands;
mod recover;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Rearrange the partitions on a disk in one restart, safely.
#[derive(Parser)]
#[command(name = "bootshift", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the disk's partitions as BootShift sees them.
    Probe {
        disk: PathBuf,
        /// Print the current layout as a target file to edit, instead of a table.
        #[arg(long)]
        target_template: bool,
    },
    /// Show the steps that turn the disk into TARGET (a JSON file), or why it is refused.
    Plan { disk: PathBuf, target: PathBuf },
    /// Run every safety check for reaching TARGET. Changes nothing.
    Check { disk: PathBuf, target: PathBuf },
    /// Check everything, then prepare the change to happen on the next restart.
    Apply {
        disk: PathBuf,
        target: PathBuf,
        /// Confirm that your data is backed up and you want to go ahead.
        #[arg(long)]
        yes: bool,
        /// The static bootshift binary to run in the recovery system. Defaults to this one.
        #[arg(long)]
        init: Option<PathBuf>,
    },
    /// Finish an interrupted run from a live USB, using the options saved on the ESP in
    /// EFI/bootshift/recovery-args.txt.
    Resume {
        disk: PathBuf,
        /// The saved kernel options, such as "bootshift.disk=… bootshift.journal=…".
        options: String,
    },
}

fn main() -> anyhow::Result<()> {
    // The kernel starts this binary as /init in the recovery system's initramfs.
    if std::process::id() == 1 {
        recover::run_as_init();
    }
    match Cli::parse().command {
        Command::Probe {
            disk,
            target_template,
        } => commands::probe(&disk, target_template),
        Command::Plan { disk, target } => commands::plan(&disk, &target),
        Command::Check { disk, target } => commands::check(&disk, &target),
        Command::Apply {
            disk,
            target,
            yes,
            init,
        } => commands::apply(&disk, &target, yes, init),
        Command::Resume { disk, options } => recover::resume(&disk, &options),
    }
}
