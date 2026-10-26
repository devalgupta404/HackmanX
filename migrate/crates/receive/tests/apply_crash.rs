//! The applier's crash test (plan.md task 4.2): a power cut at every write and flush point of a
//! migration, then resume, with the final target equal to the source. The two-device harness
//! ([`CrashBudget`]/[`CrashDevice`]) crashes the target and the scratch together and drops whatever
//! was not flushed, so this covers every durability boundary of invariant M3.

#![allow(clippy::expect_used)]

use bootshift_disk::{BlockDevice, MemDisk};
use bootshift_migrate_core::{DiskRange, Epoch, FrameSeq, LayoutMap};
use bootshift_migrate_receive::{Applier, IncomingChunk, Store};
use bootshift_migrate_testkit::{CrashBudget, CrashDevice};

const SRC_LEN: u64 = 4 << 20;
const SCRATCH_LEN: usize = 32 << 20;
const CHUNK: u64 = 256 * 1024;
const PER_BATCH: usize = 2;
const BLOCK: u32 = 4096;
const EPOCH: Epoch = Epoch(7);
const RUN_ID: u64 = 1;

fn source() -> MemDisk {
    let mut bytes = vec![0u8; SRC_LEN as usize];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (i as u64).wrapping_mul(2654435761).to_le_bytes()[0];
    }
    MemDisk::from_bytes(bytes, BLOCK).expect("source")
}

fn layout() -> LayoutMap {
    LayoutMap::new(vec![(
        DiskRange::new(0, SRC_LEN),
        DiskRange::new(0, SRC_LEN),
    )])
}

fn batches(source: &MemDisk) -> Vec<Vec<IncomingChunk>> {
    let mut chunks = Vec::new();
    let mut offset = 0u64;
    let mut seq = 1u64;
    while offset < source.len_bytes() {
        let end = (offset + CHUNK).min(source.len_bytes());
        let mut data = vec![0u8; (end - offset) as usize];
        source.read_at(offset, &mut data).expect("read source");
        let hash = *blake3::hash(&data).as_bytes();
        chunks.push(IncomingChunk {
            seq: FrameSeq(seq),
            source: DiskRange::new(offset, end),
            hash,
            data,
        });
        offset = end;
        seq += 1;
    }
    chunks.chunks(PER_BATCH).map(|c| c.to_vec()).collect()
}

fn fresh(len: usize, budget: &CrashBudget) -> CrashDevice {
    CrashDevice::new(MemDisk::zeroed(len, BLOCK).expect("disk"), budget.clone())
}

/// Applies all batches; returns `false` if a crash interrupted it.
fn apply_all(
    applier: &mut Applier,
    target: &mut CrashDevice,
    scratch: &mut CrashDevice,
    all: &[Vec<IncomingChunk>],
) -> bool {
    for batch in all {
        let mut store = Store::new(scratch);
        if applier.apply_batch(&mut store, target, batch).is_err() {
            return false;
        }
    }
    true
}

#[test]
fn crashing_at_every_point_still_recovers_the_target() {
    let source = source();
    let all = batches(&source);

    // A dry run with an unlimited budget: it completes, and counts the operations.
    let budget = CrashBudget::unlimited();
    let mut target = fresh(SRC_LEN as usize, &budget);
    let mut scratch = fresh(SCRATCH_LEN, &budget);
    let mut applier = Applier::new(RUN_ID, EPOCH, layout(), SRC_LEN);
    assert!(apply_all(&mut applier, &mut target, &mut scratch, &all));
    assert_eq!(target.image().expect("image").bytes(), source.bytes());
    let total = budget.consumed();
    assert!(total > 10, "the run should have many write/flush points");

    for step in 0..total {
        // Run until a crash at the chosen global step.
        let crash = CrashBudget::after(step);
        let mut target = fresh(SRC_LEN as usize, &crash);
        let mut scratch = fresh(SCRATCH_LEN, &crash);
        let mut applier = Applier::new(RUN_ID, EPOCH, layout(), SRC_LEN);
        let _ = apply_all(&mut applier, &mut target, &mut scratch, &all);

        // The surviving images are the last durable state.
        let target_image = target.image().expect("target image");
        let scratch_image = scratch.image().expect("scratch image");

        // Resume on fresh, uncrashed devices and finish.
        let unlimited = CrashBudget::unlimited();
        let mut target = CrashDevice::new(target_image, unlimited.clone());
        let mut scratch = CrashDevice::new(scratch_image, unlimited.clone());
        let mut resumed = {
            let store = Store::new(&mut scratch);
            Applier::resume(&store, RUN_ID, EPOCH, layout(), SRC_LEN).expect("resume")
        };
        assert!(apply_all(&mut resumed, &mut target, &mut scratch, &all));

        assert_eq!(
            target.image().expect("image").bytes(),
            source.bytes(),
            "a crash at step {step} did not recover to the source"
        );
    }
}
