//! `bootshift-migrate`: the one binary that runs as `send` on the source and `receive` inside the
//! receiver image on the target (implementation I8). The subcommands are wired here; the work
//! behind each lands phase by phase per `migrate/plan.md`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "bootshift-migrate",
    about = "Migrate a running Linux machine onto new hardware: verified, reshaped to the new disk, \
             the source never written, with a signed receipt.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Boot as the receiver on the target: print a pairing code and wait for the source.
    Receive {
        /// The new data disk the machine will run from.
        #[arg(long)]
        data: Option<PathBuf>,
        /// A small device for the receiver's journal, index and receipt.
        #[arg(long)]
        scratch: Option<PathBuf>,
        /// Instead of a scratch device, reserve this many bytes at the end of the data disk.
        #[arg(long, value_name = "BYTES")]
        scratch_end: Option<u64>,
    },
    /// Send this source to a paired receiver.
    Send {
        /// The receiver's pairing code.
        code: String,
        /// The receiver's address.
        #[arg(long)]
        to: String,
        /// Data-downtime budget for the cutover gate.
        #[arg(long, default_value = "5s")]
        budget: String,
        /// Change-capture mode.
        #[arg(long, default_value = "auto")]
        mode: String,
    },
    /// Show the passes, churn, forecast and state on both sides.
    Status,
    /// Boot a throwaway copy of the replica to test it while replication continues.
    Rehearse {
        /// Tear the rehearsal down.
        #[arg(long)]
        end: bool,
    },
    /// Freeze the source, send the last changes, verify, and commit.
    Cutover {
        /// Prove the whole target before it boots (minutes), rather than after go-live.
        #[arg(long)]
        verified_first: bool,
        /// Schedule the cutover for this time.
        #[arg(long)]
        at: Option<String>,
    },
    /// Abandon the migration and leave the source running.
    Rollback,
    /// Background re-read of the preserved source against the receipt (A10).
    Verify,
    /// Check a receipt's signature and, given both disks, its roots.
    VerifyReceipt {
        file: PathBuf,
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        target: Option<PathBuf>,
    },
    /// Lay out, grow, adapt and boot the target from a committed run.
    Finalize {
        /// Restore the guest changes recorded in the undo log.
        #[arg(long)]
        undo_adapt: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Receive { .. } => "receive",
        Command::Send { .. } => "send",
        Command::Status => "status",
        Command::Rehearse { .. } => "rehearse",
        Command::Cutover { .. } => "cutover",
        Command::Rollback => "rollback",
        Command::Verify => "verify",
        Command::VerifyReceipt { .. } => "verify-receipt",
        Command::Finalize { .. } => "finalize",
    };
    anyhow::bail!("bootshift-migrate: `{name}` is not implemented yet (see migrate/plan.md)")
}
