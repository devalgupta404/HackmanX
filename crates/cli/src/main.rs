mod inspect;

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
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Probe {
            disk,
            target_template,
        } => inspect::probe(&disk, target_template),
        Command::Plan { disk, target } => inspect::plan(&disk, &target),
        Command::Check { disk, target } => inspect::check(&disk, &target),
    }
}
