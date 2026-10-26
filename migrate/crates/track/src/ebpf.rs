//! The eBPF dirty-block tracker (implementation I5.3): load the BPF program, attach it to the block
//! request-issue tracepoint, and read its double-buffered per-CPU bitmap on drain. This is the
//! userspace half; [`tracker.bpf.c`](../bpf/tracker.bpf.c) is the kernel half. Needs root.
//!
//! It implements [`Tracker`] with the same generation handshake as [`SimTracker`](crate::SimTracker):
//! `flip` moves new writes into the next buffer, `drain` takes the one the sender has flipped past.
//! It fails closed (M4): an overflow the kernel recorded, or any map error during a drain, comes
//! back as [`Drained::Broken`] so the pass becomes a full compare rather than trusting a gap.

use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use bootshift_migrate_core::DiskRange;
use libbpf_rs::{Link, Map, MapCore, MapFlags, Object, ObjectBuilder};

use crate::tracker::{Broken, Drained, Generation, TrackError, Tracker};

/// The compiled BPF program, built by `build.rs` when the `ebpf` feature is on.
const PROGRAM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tracker.bpf.o"));

const TRACK_BLOCK: u64 = 4096;
const BITS_PER_WORD: u64 = 64;
const COUNTER_OVERFLOW: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum EbpfError {
    #[error("loading or attaching the BPF tracker: {0}")]
    Load(String),
    #[error("the BPF object has no map named {0}")]
    MissingMap(&'static str),
    #[error("the BPF object has no program named on_rq_issue")]
    MissingProgram,
    #[error("the disk is too large for the tracker's bitmap")]
    DiskTooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct EbpfTracker {
    // The link holds the attachment; dropping it detaches the program. Declared first so it drops
    // before the object it came from.
    _link: Link,
    object: Object,
    num_cpus: usize,
    dev: u32,
    disk_len: u64,
    words_per_gen: u64,
    generation: u64,
    last_overflow: u64,
}

impl EbpfTracker {
    /// Loads and attaches the tracker for the whole block device at `path` (size `disk_len` bytes).
    /// Needs CAP_BPF/CAP_SYS_ADMIN (root).
    pub fn open(path: &Path, disk_len: u64) -> Result<Self, EbpfError> {
        let rdev = std::fs::metadata(path)?.rdev();
        // The block tracepoint reports the kernel dev_t, (major << 20) | minor.
        let dev = (major_of(rdev) << 20) | minor_of(rdev);

        let disk_blocks = disk_len.div_ceil(TRACK_BLOCK);
        let words_per_gen = disk_blocks.div_ceil(BITS_PER_WORD).max(1);
        let entries = u32::try_from(words_per_gen.saturating_mul(2))
            .map_err(|_| EbpfError::DiskTooLarge)?;
        let num_cpus =
            libbpf_rs::num_possible_cpus().map_err(|e| EbpfError::Load(e.to_string()))?;

        let mut open = ObjectBuilder::default()
            .open_memory(PROGRAM)
            .map_err(|e| EbpfError::Load(e.to_string()))?;
        {
            let mut bitmap = open
                .maps_mut()
                .find(|m| m.name() == OsStr::new("bitmap_map"))
                .ok_or(EbpfError::MissingMap("bitmap_map"))?;
            bitmap
                .set_max_entries(entries)
                .map_err(|e| EbpfError::Load(e.to_string()))?;
        }
        let object = open.load().map_err(|e| EbpfError::Load(e.to_string()))?;

        let link = {
            let prog = object
                .progs_mut()
                .find(|p| p.name() == OsStr::new("on_rq_issue"))
                .ok_or(EbpfError::MissingProgram)?;
            prog.attach_tracepoint("block", "block_rq_issue")
                .map_err(|e| EbpfError::Load(e.to_string()))?
        };

        let tracker = EbpfTracker {
            _link: link,
            object,
            num_cpus,
            dev,
            disk_len,
            words_per_gen,
            generation: 0,
            last_overflow: 0,
        };
        tracker.write_config()?;
        Ok(tracker)
    }

    fn map(&self, name: &'static str) -> Result<Map<'_>, EbpfError> {
        self.object
            .maps()
            .find(|m| m.name() == OsStr::new(name))
            .ok_or(EbpfError::MissingMap(name))
    }

    fn write_config(&self) -> Result<(), EbpfError> {
        let mut value = [0u8; 16];
        value[0..4].copy_from_slice(&self.dev.to_le_bytes());
        value[4..8].copy_from_slice(&(self.generation as u32).to_le_bytes());
        value[8..16].copy_from_slice(&self.words_per_gen.to_le_bytes());
        self.map("config_map")?
            .update(&0u32.to_le_bytes(), &value, MapFlags::ANY)
            .map_err(|e| EbpfError::Load(e.to_string()))
    }

    /// The sum of the kernel's overflow counter across CPUs: writes it could not record.
    fn overflow_total(&self) -> Result<u64, EbpfError> {
        let per_cpu = self
            .map("counters_map")?
            .lookup_percpu(&COUNTER_OVERFLOW.to_le_bytes(), MapFlags::ANY)
            .map_err(|e| EbpfError::Load(e.to_string()))?
            .ok_or(EbpfError::MissingMap("counters_map"))?;
        Ok(per_cpu.iter().map(|bytes| word_of(bytes)).sum())
    }

    /// Reads one generation's buffer into coalesced ranges, clearing every word it took.
    fn drain_buffer(&self, generation: u64) -> Result<Vec<DiskRange>, EbpfError> {
        let map = self.map("bitmap_map")?;
        let base = (generation & 1) * self.words_per_gen;
        let zero: Vec<Vec<u8>> = vec![vec![0u8; 8]; self.num_cpus];
        let mut ranges = Vec::new();
        let mut run: Option<(u64, u64)> = None; // (first block, last block), inclusive

        for offset in 0..self.words_per_gen {
            let key = u32::try_from(base + offset)
                .map_err(|_| EbpfError::DiskTooLarge)?
                .to_le_bytes();
            let per_cpu = map
                .lookup_percpu(&key, MapFlags::ANY)
                .map_err(|e| EbpfError::Load(e.to_string()))?
                .ok_or(EbpfError::MissingMap("bitmap_map"))?;
            let mut word = 0u64;
            for cpu in &per_cpu {
                word |= word_of(cpu);
            }
            if word == 0 {
                continue;
            }
            for bit in 0..BITS_PER_WORD {
                if word & (1 << bit) == 0 {
                    continue;
                }
                let block = offset * BITS_PER_WORD + bit;
                run = Some(match run {
                    Some((first, last)) if block == last + 1 => (first, block),
                    Some((first, last)) => {
                        ranges.push(self.block_range(first, last));
                        (block, block)
                    }
                    None => (block, block),
                });
            }
            map.update_percpu(&key, &zero, MapFlags::ANY)
                .map_err(|e| EbpfError::Load(e.to_string()))?;
        }
        if let Some((first, last)) = run {
            ranges.push(self.block_range(first, last));
        }
        Ok(ranges)
    }

    fn block_range(&self, first: u64, last: u64) -> DiskRange {
        let start = first * TRACK_BLOCK;
        let end = ((last + 1) * TRACK_BLOCK).min(self.disk_len);
        DiskRange::new(start, end)
    }
}

impl Tracker for EbpfTracker {
    fn confirm_live(&mut self) -> Result<(), TrackError> {
        // The program is attached (we hold the link); confirm the maps answer the kernel.
        self.overflow_total()
            .map(|_| ())
            .map_err(|e| TrackError::Backend(e.to_string()))
    }

    fn flip(&mut self) -> Result<Generation, TrackError> {
        let old = self.generation;
        self.generation = old + 1;
        self.write_config()
            .map_err(|e| TrackError::Backend(e.to_string()))?;
        Ok(Generation(old))
    }

    fn drain(&mut self, generation: Generation) -> Result<Drained, TrackError> {
        if generation.0 >= self.generation {
            return Err(TrackError::WrongGeneration {
                asked: generation,
                current: Generation(self.generation),
            });
        }
        // Fail closed (M4): an overflow since the last drain, or any map error, becomes Broken so
        // the pass falls back to a full hash compare.
        match self.overflow_total() {
            Ok(total) if total > self.last_overflow => {
                self.last_overflow = total;
                return Ok(Drained::Broken(Broken::LostEvents));
            }
            Ok(_) => {}
            Err(_) => return Ok(Drained::Broken(Broken::Detached)),
        }
        match self.drain_buffer(generation.0) {
            Ok(ranges) => Ok(Drained::Ranges(ranges)),
            Err(_) => Ok(Drained::Broken(Broken::Detached)),
        }
    }
}

fn word_of(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}

// glibc's dev_t encoding, so `std::fs::metadata().rdev()` decodes without a libc dependency.
fn major_of(dev: u64) -> u32 {
    (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)) as u32
}

fn minor_of(dev: u64) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & !0xff)) as u32
}
