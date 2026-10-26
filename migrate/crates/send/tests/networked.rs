//! The migration run over the real `Link`, with a link cut mid-stream (plan.md U2: "resume, never
//! restart"). Chunks are encoded as frames, sent over a `FaultyLink`, decoded and applied by the
//! receiver, and acknowledged; the sender resends everything unacknowledged after a reconnect. The
//! cut loses in-flight frames, yet the target still ends byte-for-byte equal to the source.

#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, VecDeque};

use bootshift_disk::MemDisk;
use bootshift_migrate_core::{
    Codec, DiskRange, Epoch, FrameSeq, LayoutMap, Message, PassKind, decode, encode,
};
use bootshift_migrate_net::{Endpoint, FaultyLink, Link, Side};
use bootshift_migrate_receive::{Applier, IncomingChunk, Store};
use bootshift_migrate_send::compare_pass;
use bootshift_migrate_testkit::LiveDisk;

const LEN: u64 = 4 << 20;
const CHUNK: u64 = 1 << 20;
const BLOCK: u32 = 4096;
const EPOCH: Epoch = Epoch(7);

fn layout() -> LayoutMap {
    LayoutMap::new(vec![(DiskRange::new(0, LEN), DiskRange::new(0, LEN))])
}

/// The receiver side: decodes frames, batches chunks, applies on a `PassEnd`, and acknowledges the
/// last durable sequence.
struct Receiver<'a> {
    applier: Applier,
    scratch: &'a mut MemDisk,
    target: &'a mut MemDisk,
    batch: Vec<IncomingChunk>,
    acked: FrameSeq,
}

impl Receiver<'_> {
    /// Processes one frame; returns an `Ack` frame to send back when a pass ends.
    fn on_frame(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        let (_, seq, message) = decode(frame).ok()?;
        match message {
            Message::Chunk {
                source, hash, data, ..
            } => {
                self.batch.push(IncomingChunk {
                    seq,
                    source,
                    hash,
                    data,
                });
                None
            }
            Message::PassEnd { .. } => {
                if !self.batch.is_empty() {
                    let mut store = Store::new(self.scratch);
                    if let Ok(acked) = self.applier.apply_batch(&mut store, self.target, &self.batch)
                    {
                        self.acked = acked;
                    }
                    self.batch.clear();
                }
                encode(EPOCH, self.acked, &Message::Ack {
                    acked_seq: self.acked.0,
                })
                .ok()
            }
            _ => None,
        }
    }
}

/// Drains everything the receiver can read right now, sending any acks back.
fn pump_receiver(receiver: &mut Receiver, link: &mut Endpoint) {
    while let Ok(Some(frame)) = link.recv() {
        if let Some(ack) = receiver.on_frame(&frame) {
            link.send(ack);
        }
    }
}

#[test]
fn a_link_cut_mid_transfer_still_delivers_a_byte_exact_target() {
    // A frozen source.
    let mut source = LiveDisk::new(LEN as usize, BLOCK, 2026).expect("source");
    for _ in 0..40 {
        source.tick().expect("seed");
    }

    let mut target = MemDisk::zeroed(LEN as usize, BLOCK).expect("target");
    let mut scratch = MemDisk::zeroed(32 << 20, BLOCK).expect("scratch");
    let chunks = LEN / CHUNK;

    // The bulk pass: every chunk differs from the empty target.
    let diffs = compare_pass(&source, &[], CHUNK).expect("compare");
    assert_eq!(diffs.len(), chunks as usize);

    let (mut send_link, mut recv_link, control) = FaultyLink::pair();
    assert_eq!(send_link.side(), Side::A);

    let mut receiver = Receiver {
        applier: Applier::new(1, EPOCH, layout(), LEN),
        scratch: &mut scratch,
        target: &mut target,
        batch: Vec::new(),
        acked: FrameSeq(0),
    };

    // Every chunk by sequence number (from 1), the authority the sender resends from.
    let last_seq = diffs.len() as u64;
    let chunk_by_seq: BTreeMap<u64, Message> = diffs
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            (i as u64 + 1, Message::Chunk {
                pass: 1,
                source: chunk.source,
                hash: chunk.hash,
                codec: Codec::None,
                data: chunk.data.clone(),
            })
        })
        .collect();
    let pass_end_seq = FrameSeq(last_seq + 1);
    let pass_end = Message::PassEnd {
        pass: 1,
        kind: PassKind::Bulk,
        sent_bytes: 0,
        dirty_at_end: 0,
    };

    // The send queue: every chunk still unacknowledged, in order, then the PassEnd. Rebuilt from
    // `chunk_by_seq` whenever the sender (re)starts a pass, so a cut can never strand a chunk.
    let build_queue = |acked: u64| -> VecDeque<(FrameSeq, Message)> {
        let mut queue: VecDeque<(FrameSeq, Message)> = chunk_by_seq
            .range((acked + 1)..)
            .map(|(seq, message)| (FrameSeq(*seq), message.clone()))
            .collect();
        queue.push_back((pass_end_seq, pass_end.clone()));
        queue
    };

    let mut acked = 0u64;
    let mut queue = build_queue(acked);
    let mut sent = 0usize;
    let cut_after = (chunks as usize) / 2; // cut halfway through the chunks
    let mut cut_done = false;

    for _ in 0..1000 {
        if acked >= last_seq && queue.is_empty() {
            break;
        }

        // Cut once, mid-stream, then reconnect and resend every chunk not yet acknowledged.
        if !cut_done && sent == cut_after {
            cut_done = true;
            control.cut();
            control.reconnect();
            queue = build_queue(acked);
        }

        // Sender sends the next frame.
        if let Some((seq, message)) = queue.pop_front() {
            if let Ok(frame) = encode(EPOCH, seq, &message) {
                send_link.send(frame);
            }
            sent += 1;
        }

        pump_receiver(&mut receiver, &mut recv_link);

        // Sender reads acks.
        while let Ok(Some(frame)) = send_link.recv() {
            if let Ok((_, _, Message::Ack { acked_seq })) = decode(&frame) {
                acked = acked.max(acked_seq);
            }
        }
    }

    assert!(cut_done, "the test should have exercised a cut");
    assert_eq!(acked, last_seq, "every chunk was acknowledged");
    assert_eq!(
        target.bytes(),
        source.snapshot().as_slice(),
        "the target matches the source despite the mid-transfer cut"
    );
}
