//! `bootshift-migrate`: the one binary that runs as `send` on the source and `receive` inside the
//! receiver image on the target (implementation I8). The subcommands are wired here; the work
//! behind each lands phase by phase per `migrate/plan.md`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod common;
mod receive;
mod send;

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
    /// Boot as the receiver on the target: wait for the source and apply what it sends.
    Receive {
        /// The new data disk the machine will run from.
        #[arg(long)]
        data: PathBuf,
        /// A small device for the receiver's journal, index and receipt.
        #[arg(long)]
        scratch: PathBuf,
        /// Address to listen on for the source.
        #[arg(long, default_value = "0.0.0.0:9876")]
        listen: String,
        /// Block size to assume when the data or scratch is an image file, not a block device.
        #[arg(long, default_value_t = 4096)]
        block_size: u32,
        /// Migration epoch (must match the sender).
        #[arg(long, default_value_t = 1)]
        epoch: u64,
        /// Run id for the scratch journal.
        #[arg(long, default_value_t = 1)]
        run_id: u64,
    },
    /// Send this source to a paired receiver.
    Send {
        /// The source disk or image to send (opened read-only).
        #[arg(long)]
        source: PathBuf,
        /// The receiver's address, host:port.
        #[arg(long)]
        to: String,
        /// Block size to assume when the source is an image file, not a block device.
        #[arg(long, default_value_t = 4096)]
        block_size: u32,
        /// Migration epoch (must match the receiver).
        #[arg(long, default_value_t = 1)]
        epoch: u64,
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
    match cli.command {
        Command::Receive {
            data,
            scratch,
            listen,
            block_size,
            epoch,
            run_id,
        } => receive::run(&data, &scratch, &listen, block_size, epoch, run_id),
        Command::Send {
            source,
            to,
            block_size,
            epoch,
        } => send::run(&source, &to, block_size, epoch),
        Command::Status => not_implemented("status"),
        Command::Rehearse { .. } => not_implemented("rehearse"),
        Command::Cutover { .. } => not_implemented("cutover"),
        Command::Rollback => not_implemented("rollback"),
        Command::Verify => not_implemented("verify"),
        Command::VerifyReceipt { .. } => not_implemented("verify-receipt"),
        Command::Finalize { .. } => not_implemented("finalize"),
    }
}

fn not_implemented(name: &str) -> anyhow::Result<()> {
    anyhow::bail!("bootshift-migrate: `{name}` is not implemented yet (see migrate/plan.md)")
}
