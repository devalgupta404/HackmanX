//! Compare mode end to end, in memory (plan.md task 2.4 / Phase 5–7 data path): a `LiveDisk`
//! source keeps changing while compare passes copy the differences to the target; once the source
//! is "frozen" (no more writes), the passes converge and the target equals the source byte for
//! byte. This exercises the sender's compare pass, the receiver's applier and store, and the
//! convergence shape (bulk pass, then shrinking delta passes), all without a kernel or a network.

#![allow(clippy::expect_used)]

use bootshift_disk::{BlockDevice, MemDisk};
use bootshift_migrate_core::{DiskRange, Epoch, FrameSeq, LayoutMap};
use bootshift_migrate_receive::{Applier, IncomingChunk, Store};
use bootshift_migrate_send::compare_pass;
use bootshift_migrate_testkit::LiveDisk;

const LEN: u64 = 4 << 20;
const CHUNK: u64 = 1 << 20;
const BLOCK: u32 = 4096;
const EPOCH: Epoch = Epoch(7);
const RUN_ID: u64 = 1;

fn layout() -> LayoutMap {
    LayoutMap::new(vec![(DiskRange::new(0, LEN), DiskRange::new(0, LEN))])
}

/// One pass: read the receiver's index, compare it against the source, apply the differences, and
/// return how many chunks were sent.
fn run_pass(
    source: &dyn BlockDevice,
    applier: &mut Applier,
    target: &mut MemDisk,
    scratch: &mut MemDisk,
    seq: &mut u64,
    chunks: u64,
) -> usize {
    let hashes: Vec<[u8; 32]> = {
        let store = Store::new(scratch);
        (0..chunks)
            .map(|i| store.chunk_hash(i).expect("chunk hash"))
            .collect()
    };
    let diffs = compare_pass(source, &hashes, CHUNK).expect("compare");
    if diffs.is_empty() {
        return 0;
    }
    let batch: Vec<IncomingChunk> = diffs
        .iter()
        .map(|chunk| {
            *seq += 1;
            IncomingChunk {
                seq: FrameSeq(*seq),
                source: chunk.source,
                hash: chunk.hash,
                data: chunk.data.clone(),
            }
        })
        .collect();
    let sent = batch.len();
    let mut store = Store::new(scratch);
    applier
        .apply_batch(&mut store, target, &batch)
        .expect("apply");
    sent
}

#[test]
fn compare_mode_converges_and_the_target_matches_the_source_at_freeze() {
    let mut source = LiveDisk::new(LEN as usize, BLOCK, 2026).expect("source");
    for _ in 0..40 {
        source.tick().expect("seed write");
    }

    let mut target = MemDisk::zeroed(LEN as usize, BLOCK).expect("target");
    let mut scratch = MemDisk::zeroed(32 << 20, BLOCK).expect("scratch");
    let mut applier = Applier::new(RUN_ID, EPOCH, layout(), LEN);
    let chunks = LEN / CHUNK;
    let mut seq = 0u64;

    // The bulk pass sends every chunk (the target starts empty).
    let bulk = run_pass(
        &source,
        &mut applier,
        &mut target,
        &mut scratch,
        &mut seq,
        chunks,
    );
    assert_eq!(bulk, chunks as usize, "bulk pass sends every chunk");

    // Delta passes while the source keeps being written: each should be smaller than the bulk.
    for _ in 0..6 {
        for _ in 0..3 {
            source.tick().expect("live write");
        }
        let delta = run_pass(
            &source,
            &mut applier,
            &mut target,
            &mut scratch,
            &mut seq,
            chunks,
        );
        assert!(delta <= chunks as usize);
    }

    // Freeze: no more writes. The passes must converge to zero.
    let mut passes = 0;
    loop {
        let sent = run_pass(
            &source,
            &mut applier,
            &mut target,
            &mut scratch,
            &mut seq,
            chunks,
        );
        passes += 1;
        assert!(passes < 8, "it should converge quickly once frozen");
        if sent == 0 {
            break;
        }
    }

    assert_eq!(
        target.bytes(),
        source.snapshot().as_slice(),
        "the target equals the frozen source byte for byte"
    );
}
