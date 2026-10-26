//! A two-device crash harness (plan.md task 2.4). A [`CrashBudget`] is shared by the devices of a
//! run, so a crash at a chosen global step hits the target and the scratch together — the way a
//! power cut does. A [`CrashDevice`] buffers writes until [`flush`](bootshift_disk::BlockDevice::flush),
//! and a crash **drops whatever has not been flushed**, so the surviving image is exactly the last
//! durable state. Iterating the budget over every write and flush of a run tests that the applier
//! resumes to a correct target from any interruption point (invariant M3).

use std::cell::Cell;
use std::rc::Rc;

use bootshift_disk::{BlockDevice, DiskError, MemDisk};

/// A crash budget shared by the devices of one run. Every write or flush consumes one step; once
/// the budget is spent, the next operation fails, modelling a power cut at that global step.
#[derive(Clone)]
pub struct CrashBudget(Rc<Cell<i64>>);

impl CrashBudget {
    /// Crashes on the operation after `steps` have succeeded. `after(0)` crashes on the first.
    pub fn after(steps: i64) -> Self {
        CrashBudget(Rc::new(Cell::new(steps)))
    }

    /// Never crashes, but counts the operations, so a dry run can report how many steps a run has.
    pub fn unlimited() -> Self {
        CrashBudget(Rc::new(Cell::new(i64::MAX)))
    }

    /// How many operations an unlimited budget has seen.
    pub fn consumed(&self) -> i64 {
        i64::MAX - self.0.get()
    }

    fn tick(&self) -> bool {
        let left = self.0.get();
        self.0.set(left - 1);
        left <= 0
    }
}

/// A `MemDisk` whose unflushed writes are lost on a crash.
pub struct CrashDevice {
    inner: MemDisk,
    pending: Vec<(u64, Vec<u8>)>,
    budget: CrashBudget,
}

impl CrashDevice {
    pub fn new(inner: MemDisk, budget: CrashBudget) -> Self {
        CrashDevice {
            inner,
            pending: Vec::new(),
            budget,
        }
    }

    /// The last durable image: a fresh disk holding only what has been flushed. Unflushed writes
    /// are not in it — exactly what survives a power cut. The geometry matches the inner disk, so
    /// this only errors if that disk was somehow invalid.
    pub fn image(&self) -> Result<MemDisk, DiskError> {
        MemDisk::from_bytes(self.inner.bytes().to_vec(), self.inner.logical_block_size())
    }
}

fn crashed(offset: u64) -> DiskError {
    DiskError::Io {
        offset,
        source: std::io::Error::other("simulated power cut"),
    }
}

impl BlockDevice for CrashDevice {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }

    fn logical_block_size(&self) -> u32 {
        self.inner.logical_block_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        // Reads see the durable image; the applier only reads back after it has flushed.
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        if self.budget.tick() {
            return Err(crashed(offset));
        }
        self.pending.push((offset, data.to_vec()));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        if self.budget.tick() {
            return Err(crashed(0));
        }
        for (offset, data) in self.pending.drain(..) {
            self.inner.write_at(offset, &data)?;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unflushed_writes_are_lost_on_a_crash() {
        let budget = CrashBudget::after(1); // one write succeeds, the flush crashes
        let mut device = CrashDevice::new(MemDisk::zeroed(8192, 4096).expect("disk"), budget);
        device.write_at(0, &[7u8; 4096]).expect("write buffered");
        assert!(device.flush().is_err(), "the flush crashes");
        // The image holds zeros: the write was never flushed.
        assert_eq!(device.image().expect("image").bytes(), &[0u8; 8192]);
    }

    #[test]
    fn flushed_writes_survive() {
        let budget = CrashBudget::after(5);
        let mut device = CrashDevice::new(MemDisk::zeroed(8192, 4096).expect("disk"), budget);
        device.write_at(0, &[7u8; 4096]).expect("write");
        device.flush().expect("flush");
        let image = device.image().expect("image");
        assert_eq!(&image.bytes()[0..4096], &[7u8; 4096]);
    }

    #[test]
    fn a_shared_budget_crashes_both_devices_at_one_global_step() {
        let budget = CrashBudget::after(2);
        let mut a = CrashDevice::new(MemDisk::zeroed(4096, 4096).expect("a"), budget.clone());
        let mut b = CrashDevice::new(MemDisk::zeroed(4096, 4096).expect("b"), budget.clone());
        assert!(a.write_at(0, &[1u8; 4096]).is_ok()); // step 1
        assert!(b.write_at(0, &[2u8; 4096]).is_ok()); // step 2
        assert!(a.write_at(0, &[3u8; 4096]).is_err(), "step 3 crashes both");
    }
}
