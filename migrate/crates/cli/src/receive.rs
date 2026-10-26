//! `receive`: listen for the source, and apply every chunk it sends to the target disk, verifying
//! each against the sender's hash before it is recorded (M3), then acknowledge the last durable
//! sequence. A crash resumes from the newest scratch slot; the sender resends from the last ack, so
//! a batch already applied is a no-op (A4.2). The journal and index live on the scratch device.

use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use bootshift_disk::{Access, BlockDevice, RealDevice};
use bootshift_migrate_core::{
    DiskRange, Epoch, FrameSeq, LayoutMap, Message, PROTOCOL_VERSION, Role, decode, encode,
};
use bootshift_migrate_net::{Link, LinkError, TcpLink};
use bootshift_migrate_receive::{Applier, IncomingChunk, Store};

use crate::common::open_device;

/// Apply once this many chunk bytes have gathered, or at the end of a pass.
const BATCH_BYTES: usize = 16 << 20;

pub fn run(
    data: &Path,
    scratch: &Path,
    listen: &str,
    block_size: u32,
    epoch: u64,
    run_id: u64,
) -> Result<()> {
    let mut target = open_device(data, Access::ReadWrite, block_size)?;
    let mut scratch_dev = open_device(scratch, Access::ReadWrite, block_size)?;
    let data_len = target.len_bytes();
    let epoch = Epoch(epoch);
    let layout = LayoutMap::new(vec![(DiskRange::new(0, data_len), DiskRange::new(0, data_len))]);
    let mut applier = {
        let store = Store::new(&mut scratch_dev);
        Applier::resume(&store, run_id, epoch, layout, data_len)
            .context("reading the newest scratch slot")?
    };

    let listener = TcpListener::bind(listen).with_context(|| format!("binding {listen}"))?;
    println!(
        "receive: listening on {listen}; target {} ({data_len} bytes), resuming from seq {}",
        data.display(),
        applier.acked().0
    );
    let mut link = TcpLink::accept(&listener).context("accepting the source")?;

    let mut batch: Vec<IncomingChunk> = Vec::new();
    let mut batch_bytes = 0usize;
    loop {
        match link.recv() {
            Ok(Some(frame)) => {
                let (_, seq, message) = decode(&frame).context("decoding a frame")?;
                match message {
                    Message::Hello { .. } => {
                        link.send(encode(epoch, FrameSeq(0), &Message::Hello {
                            protocol: PROTOCOL_VERSION,
                            role: Role::Receiver,
                            software: concat!("bootshift-migrate ", env!("CARGO_PKG_VERSION"))
                                .to_string(),
                            disk_len: data_len,
                            block_size: target.logical_block_size(),
                            used_digest: [0u8; 32],
                        })?);
                    }
                    Message::Chunk {
                        source, hash, data, ..
                    } => {
                        batch_bytes += data.len();
                        batch.push(IncomingChunk {
                            seq,
                            source,
                            hash,
                            data,
                        });
                        if batch_bytes >= BATCH_BYTES {
                            apply(&mut applier, &mut scratch_dev, &mut target, &mut batch, &mut link, epoch)?;
                            batch_bytes = 0;
                        }
                    }
                    Message::PassEnd { .. } => {
                        apply(&mut applier, &mut scratch_dev, &mut target, &mut batch, &mut link, epoch)?;
                        batch_bytes = 0;
                        println!(
                            "receive: pass complete, acknowledged through seq {}",
                            applier.acked().0
                        );
                    }
                    Message::Committed { .. } => {
                        println!("receive: source committed; the target is written and verified");
                        return Ok(());
                    }
                    Message::Error { reason, fix } => {
                        anyhow::bail!("the source reported an error: {reason} (fix: {fix})");
                    }
                    _ => {}
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(LinkError::Cut) => {
                println!(
                    "receive: the source disconnected, acknowledged through seq {}",
                    applier.acked().0
                );
                return Ok(());
            }
        }
    }
}

/// Applies the gathered batch (write, flush, read-back verify, re-hash index, commit) and sends the
/// acknowledgement for the last durable sequence.
fn apply(
    applier: &mut Applier,
    scratch: &mut RealDevice,
    target: &mut RealDevice,
    batch: &mut Vec<IncomingChunk>,
    link: &mut TcpLink,
    epoch: Epoch,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let acked = {
        let mut store = Store::new(scratch);
        applier
            .apply_batch(&mut store, target, batch)
            .context("applying and verifying a batch")?
    };
    batch.clear();
    link.send(encode(epoch, acked, &Message::Ack { acked_seq: acked.0 })?);
    Ok(())
}
