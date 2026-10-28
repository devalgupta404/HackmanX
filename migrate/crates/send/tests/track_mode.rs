//! The sender pass loop (I5.1) driven against a deterministic in-memory transfer: a seeded
//! `LiveDisk` source that keeps changing while the migration runs, a `SimTracker` the sink feeds
//! its writes into, and an `Applier` placing chunks on a target `MemDisk`. The loop flips the
//! tracker before each read and drains it after, folds the drained writes back into the dirty set,
//! and asks the convergence model when to stop. These tests prove the orchestration: it reaches a
//! cutover-ready point and the target is byte-exact, it falls back to a hash compare when the
//! tracker breaks (M4) and still converges, and it surfaces the model's verdict instead of looping
//! for ever when churn outruns the link.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use bootshift_disk::{BlockDevice, DiskError, MemDisk};
use bootshift_migrate_core::{Advice, ConvergenceModel, DiskRange, Epoch, FrameSeq, LayoutMap};
use bootshift_migrate_receive::{Applier, IncomingChunk, Store};
use bootshift_migrate_send::{Outcome, Transfer, Transferred, compare_pass, run_passes};
use bootshift_migrate_testkit::LiveDisk;
use bootshift_migrate_track::{Drained, Generation, SimTracker, TrackError, Tracker};

const LEN: u64 = 8 << 20;
const CHUNK: u64 = 1 << 20;
const BLOCK: u32 = 4096;
const EPOCH: Epoch = Epoch(7);
const FAST_LINK: u64 = 1_000_000_000;

fn layout() -> LayoutMap {
    LayoutMap::new(vec![(DiskRange::new(0, LEN), DiskRange::new(0, LEN))])
}

/// A tracker shared between the pass loop (which flips and drains it) and the transfer sink (which
/// records the writes it makes during a pass). The borrows never overlap in time.
#[derive(Clone)]
struct SharedTracker(Rc<RefCell<SimTracker>>);

impl Tracker for SharedTracker {
    fn confirm_live(&mut self) -> Result<(), TrackError> {
        self.0.borrow_mut().confirm_live()
    }
    fn flip(&mut self) -> Result<Generation, TrackError> {
        self.0.borrow_mut().flip()
    }
    fn drain(&mut self, generation: Generation) -> Result<Drained, TrackError> {
        self.0.borrow_mut().drain(generation)
    }
}

/// The in-memory transfer: reads the source, places chunks on the target through the applier, and
/// models writes that land during a pass by ticking the source and recording them in the tracker.
struct MemTransfer {
    source: LiveDisk,
    target: MemDisk,
    scratch: MemDisk,
    applier: Applier,
    tracker: Rc<RefCell<SimTracker>>,
    seq: u64,
    /// The effective link rate the sink reports, in bytes per second.
    rate: u64,
    /// Writes to issue during each `send_ranges`, modelling a busy source.
    churn: usize,
    /// How much `churn` grows each pass (a source whose write rate climbs).
    churn_growth: usize,
}

impl MemTransfer {
    fn new(
        seed: u64,
        rate: u64,
        churn: usize,
        churn_growth: usize,
        tracker: Rc<RefCell<SimTracker>>,
    ) -> Self {
        MemTransfer {
            source: LiveDisk::new(LEN as usize, BLOCK, seed).expect("source"),
            target: MemDisk::zeroed(LEN as usize, BLOCK).expect("target"),
            scratch: MemDisk::zeroed(32 << 20, BLOCK).expect("scratch"),
            applier: Applier::new(1, EPOCH, layout(), LEN),
            tracker,
            seq: 0,
            rate,
            churn,
            churn_growth,
        }
    }

    fn place(&mut self, batch: &[IncomingChunk]) {
        if batch.is_empty() {
            return;
        }
        let mut store = Store::new(&mut self.scratch);
        self.applier
            .apply_batch(&mut store, &mut self.target, batch)
            .expect("apply_batch");
    }
}

impl Transfer for MemTransfer {
    fn send_ranges(&mut self, ranges: &[DiskRange]) -> Result<Transferred, DiskError> {
        let mut batch = Vec::new();
        let mut bytes = 0u64;
        for range in ranges {
            let len = (range.end - range.start) as usize;
            let mut data = vec![0u8; len];
            self.source.read_at(range.start, &mut data)?;
            let hash = *blake3::hash(&data).as_bytes();
            self.seq += 1;
            bytes += len as u64;
            batch.push(IncomingChunk {
                seq: FrameSeq(self.seq),
                source: *range,
                hash,
                data,
            });
        }
        self.place(&batch);

        // Writes that land during the pass, recorded in the tracker's live generation.
        for _ in 0..self.churn {
            let written = self.source.tick()?;
            self.tracker.borrow_mut().record_write(written);
        }
        self.churn += self.churn_growth;

        Ok(Transferred {
            bytes,
            rate: self.rate,
            duration: Duration::from_millis(1),
        })
    }

    fn compare_all(&mut self) -> Result<Transferred, DiskError> {
        // The receiver's index: the hash of each 1 MiB target chunk.
        let chunks = self.target.len_bytes().div_ceil(CHUNK);
        let mut hashes = Vec::with_capacity(chunks as usize);
        for index in 0..chunks {
            let start = index * CHUNK;
            let end = ((index + 1) * CHUNK).min(self.target.len_bytes());
            let mut buf = vec![0u8; (end - start) as usize];
            self.target.read_at(start, &mut buf)?;
            hashes.push(*blake3::hash(&buf).as_bytes());
        }

        let diffs = compare_pass(&self.source, &hashes, CHUNK)?;
        let mut batch = Vec::new();
        let mut bytes = 0u64;
        for diff in diffs {
            self.seq += 1;
            bytes += diff.source.end - diff.source.start;
            batch.push(IncomingChunk {
                seq: FrameSeq(self.seq),
                source: diff.source,
                hash: diff.hash,
                data: diff.data,
            });
        }
        self.place(&batch);

        Ok(Transferred {
            bytes,
            rate: self.rate,
            duration: Duration::from_millis(1),
        })
    }

    fn unacknowledged(&mut self) -> Vec<DiskRange> {
        // The applier acknowledges synchronously, so nothing is ever left outstanding here.
        Vec::new()
    }
}

#[test]
fn converges_to_a_cutover_and_a_final_compare_makes_the_target_byte_exact() {
    let tracker = Rc::new(RefCell::new(SimTracker::new()));
    let mut sink = MemTransfer::new(2026, FAST_LINK, 2, 0, Rc::clone(&tracker));

    // Some writes that accumulated before the migration began (the first bulk drain sees them).
    for _ in 0..6 {
        let written = sink.source.tick().expect("pre-churn");
        tracker.borrow_mut().record_write(written);
    }

    let mut model = ConvergenceModel::new(Duration::from_secs(10), Duration::ZERO, false);
    let mut driver = SharedTracker(Rc::clone(&tracker));
    let outcome = run_passes(
        &mut sink,
        &mut driver,
        &[DiskRange::new(0, LEN)],
        &mut model,
        50,
    )
    .expect("run_passes");

    assert!(
        matches!(outcome, Outcome::Ready { .. }),
        "a light, steady churn converges to a cutover-ready point: {outcome:?}"
    );

    // Freeze and run the final pass: no more writes, one hash compare, and the target is exact.
    sink.compare_all().expect("final compare");
    assert_eq!(
        sink.target.bytes(),
        sink.source.snapshot().as_slice(),
        "after the final pass the target equals the frozen source byte for byte"
    );
}

#[test]
fn a_broken_tracker_falls_back_to_compare_and_still_converges() {
    let tracker = Rc::new(RefCell::new(SimTracker::new()));
    let mut sink = MemTransfer::new(7, FAST_LINK, 3, 0, Rc::clone(&tracker));

    // Tracking is broken from the start: the first drain reports it, and the loop must switch to a
    // full hash compare rather than trust the tracker (M4).
    tracker
        .borrow_mut()
        .break_tracking(bootshift_migrate_track::Broken::Detached);

    let mut model = ConvergenceModel::new(Duration::from_secs(10), Duration::ZERO, false);
    let mut driver = SharedTracker(Rc::clone(&tracker));
    let outcome = run_passes(
        &mut sink,
        &mut driver,
        &[DiskRange::new(0, LEN)],
        &mut model,
        50,
    )
    .expect("run_passes");

    assert!(
        matches!(outcome, Outcome::Ready { .. }),
        "the compare fallback still reaches a cutover-ready point: {outcome:?}"
    );
    assert_eq!(
        sink.target.bytes(),
        sink.source.snapshot().as_slice(),
        "the compare pass synced the target to the source exactly"
    );
}

#[test]
fn churn_that_outruns_the_link_asks_for_a_decision() {
    let tracker = Rc::new(RefCell::new(SimTracker::new()));
    // A slow link (1 KB/s) and growing churn, so the dirty set never fits the budget.
    let mut sink = MemTransfer::new(99, 1000, 8, 8, Rc::clone(&tracker));

    for _ in 0..8 {
        let written = sink.source.tick().expect("pre-churn");
        tracker.borrow_mut().record_write(written);
    }

    // A tiny budget against a slow link: predicted downtime stays over budget and never shrinks.
    let mut model = ConvergenceModel::new(Duration::from_millis(1), Duration::ZERO, false);
    let mut driver = SharedTracker(Rc::clone(&tracker));
    let outcome = run_passes(
        &mut sink,
        &mut driver,
        &[DiskRange::new(0, LEN)],
        &mut model,
        50,
    )
    .expect("run_passes");

    assert!(
        matches!(outcome, Outcome::NeedsDecision(_)),
        "the loop surfaces the model's verdict instead of looping for ever: {outcome:?}"
    );
    if let Outcome::NeedsDecision(advice) = outcome {
        assert!(
            matches!(
                advice,
                Advice::OfferThrottle { .. } | Advice::CannotConverge { .. } | Advice::OfferMirror
            ),
            "it asks for a throttle, a mirror, or reports it cannot converge: {advice:?}"
        );
    }
}
