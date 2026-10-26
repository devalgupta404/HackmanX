//! `send`: read the source read-only, and stream it to a paired receiver over the link. Against a
//! blank target every chunk differs, so this is a full verified copy; the compare-against-index
//! optimisation (skip chunks the receiver already has) and the tracker-driven delta passes land on
//! top of this same loop. The source is never written (M1).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bootshift_disk::{Access, BlockDevice};
use bootshift_migrate_core::{
    Codec, DiskRange, Epoch, FrameSeq, Message, PROTOCOL_VERSION, PassKind, Role, decode, encode,
};
use bootshift_migrate_net::{Link, LinkError, TcpLink};

use crate::common::{open_device, recv_message};

const CHUNK: u64 = 1 << 20;
/// Stop framing new chunks while this many bytes are still queued, so a slow receiver bounds the
/// sender's memory instead of letting the outgoing buffer grow without limit.
const HIGH_WATER: usize = 32 << 20;

pub fn run(source: &Path, to: &str, block_size: u32, epoch: u64) -> Result<()> {
    let src = open_device(source, Access::ReadOnly, block_size)?;
    let src_len = src.len_bytes();
    let epoch = Epoch(epoch);

    let mut link = TcpLink::connect(to).with_context(|| format!("connecting to {to}"))?;
    link.send(encode(epoch, FrameSeq(0), &Message::Hello {
        protocol: PROTOCOL_VERSION,
        role: Role::Sender,
        software: concat!("bootshift-migrate ", env!("CARGO_PKG_VERSION")).to_string(),
        disk_len: src_len,
        block_size: src.logical_block_size(),
        used_digest: [0u8; 32],
    })?);

    let (_, _, hello) = recv_message(&mut link, Duration::from_secs(30))?;
    let Message::Hello {
        disk_len: target_len,
        ..
    } = hello
    else {
        bail!("the receiver did not greet with Hello");
    };
    if target_len < src_len {
        bail!(
            "the target disk ({target_len} bytes) is smaller than the source ({src_len} bytes); \
             refusing before writing a byte"
        );
    }

    let mut acked = 0u64;
    let mut seq = 0u64;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut offset = 0u64;
    while offset < src_len {
        let end = (offset + CHUNK).min(src_len);
        let len = (end - offset) as usize;
        src.read_at(offset, &mut buf[..len])
            .with_context(|| format!("reading the source at offset {offset}"))?;
        let hash = *blake3::hash(&buf[..len]).as_bytes();
        seq += 1;
        link.send(encode(epoch, FrameSeq(seq), &Message::Chunk {
            pass: 1,
            source: DiskRange::new(offset, end),
            hash,
            codec: Codec::None,
            data: buf[..len].to_vec(),
        })?);
        offset = end;
        drain_to_high_water(&mut link, &mut acked)?;
    }

    let last_seq = seq;
    seq += 1;
    link.send(encode(epoch, FrameSeq(seq), &Message::PassEnd {
        pass: 1,
        kind: PassKind::Final,
        sent_bytes: src_len,
        dirty_at_end: 0,
    })?);
    link.flush(Duration::from_secs(120))
        .map_err(|_| anyhow::anyhow!("the link cut while flushing the final chunks"))?;

    wait_for_ack(&mut link, last_seq, Duration::from_secs(600))?;

    seq += 1;
    link.send(encode(epoch, FrameSeq(seq), &Message::Committed { epoch: epoch.0 })?);
    link.flush(Duration::from_secs(30))
        .map_err(|_| anyhow::anyhow!("the link cut before the commit was delivered"))?;

    println!("send: copied and verified {src_len} bytes to {to}");
    Ok(())
}

/// Reads acknowledgements and blocks while the outgoing buffer is above the high-water mark.
fn drain_to_high_water(link: &mut TcpLink, acked: &mut u64) -> Result<()> {
    loop {
        match link.recv() {
            Ok(Some(frame)) => {
                if let Ok((_, _, Message::Ack { acked_seq })) = decode(&frame) {
                    *acked = (*acked).max(acked_seq);
                }
            }
            Ok(None) => {
                if link.pending() <= HIGH_WATER {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(LinkError::Cut) => bail!("the link cut mid-transfer"),
        }
    }
}

fn wait_for_ack(link: &mut TcpLink, target: u64, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match link.recv() {
            Ok(Some(frame)) => {
                if let Ok((_, _, Message::Ack { acked_seq })) = decode(&frame) {
                    if acked_seq >= target {
                        return Ok(());
                    }
                }
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the receiver to acknowledge through seq {target}");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(LinkError::Cut) => bail!("the link cut before the final acknowledgement"),
        }
    }
}
