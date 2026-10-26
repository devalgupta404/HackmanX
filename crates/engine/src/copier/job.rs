use std::ops::Range;

pub use bootshift_core::scope::CopyScope;
use bootshift_core::scope::merge;

pub const MAX_CHUNK: u64 = 64 << 20;

/// Move `len` bytes from device offset `source` to `destination`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveJob {
    pub source: u64,
    pub destination: u64,
    pub len: u64,
    pub scope: CopyScope,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("a move must go somewhere, have a length, and be aligned to {block_size}-byte blocks")]
pub struct InvalidJob {
    pub block_size: u64,
}

/// How a move is cut into chunks.
///
/// A chunk is never longer than the move distance, so its destination never overlaps its own
/// source and a half-written chunk can simply be copied again. Chunks run in the direction of
/// the move (first chunk first when moving left, last chunk first when moving right), so every
/// write lands on source data that has already been copied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    job: MoveJob,
    block_size: u64,
    chunk_len: u64,
}

impl ChunkPlan {
    pub fn new(job: MoveJob, block_size: u32, max_chunk: u64) -> Result<ChunkPlan, InvalidJob> {
        let block_size = u64::from(block_size);
        let invalid = InvalidJob { block_size };
        let distance = job.source.abs_diff(job.destination);
        let aligned = [job.source, job.destination, job.len]
            .iter()
            .all(|value| value % block_size == 0);
        if distance == 0 || job.len == 0 || !aligned || max_chunk < block_size {
            return Err(invalid);
        }
        let chunk_len = distance.min(max_chunk);
        Ok(ChunkPlan {
            job,
            block_size,
            chunk_len: chunk_len - chunk_len % block_size,
        })
    }

    pub fn job(&self) -> &MoveJob {
        &self.job
    }

    pub fn chunk_len(&self) -> u64 {
        self.chunk_len
    }

    pub fn chunk_count(&self) -> u64 {
        self.job.len.div_ceil(self.chunk_len)
    }

    fn moves_left(&self) -> bool {
        self.job.destination < self.job.source
    }

    /// The bytes chunk `index` covers, relative to the partition start.
    pub fn chunk(&self, index: u64) -> Range<u64> {
        let offset = index.saturating_mul(self.chunk_len).min(self.job.len);
        if self.moves_left() {
            offset..(offset + self.chunk_len).min(self.job.len)
        } else {
            let end = self.job.len - offset;
            end.saturating_sub(self.chunk_len)..end
        }
    }

    /// The block-aligned pieces of chunk `index` that hold data. Empty when there is nothing
    /// to copy.
    pub fn pieces(&self, index: u64) -> Vec<Range<u64>> {
        let chunk = self.chunk(index);
        match &self.job.scope {
            CopyScope::Everything => vec![chunk],
            CopyScope::UsedOnly(used) => {
                let block = self.block_size;
                let pieces = used
                    .iter()
                    .filter(|r| r.start < chunk.end && chunk.start < r.end)
                    .map(|r| {
                        let start = (r.start - r.start % block).max(chunk.start);
                        let end = r.end.next_multiple_of(block).min(chunk.end);
                        start..end
                    })
                    .collect();
                merge(pieces)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn plan(source: u64, destination: u64, len: u64, scope: CopyScope) -> ChunkPlan {
        let job = MoveJob {
            source,
            destination,
            len,
            scope,
        };
        ChunkPlan::new(job, 512, MAX_CHUNK).unwrap()
    }

    #[test]
    fn chunk_is_no_longer_than_the_move_distance() {
        let plan = plan(10 * MIB, 9 * MIB, 4 * MIB, CopyScope::Everything);
        assert_eq!(plan.chunk_len(), MIB);
        assert_eq!(plan.chunk_count(), 4);
    }

    #[test]
    fn chunk_is_capped_for_long_moves() {
        let plan = plan(0, 500 * MIB, 200 * MIB, CopyScope::Everything);
        assert_eq!(plan.chunk_len(), MAX_CHUNK);
    }

    #[test]
    fn moving_left_copies_the_first_chunk_first() {
        let plan = plan(10 * MIB, 9 * MIB, 2 * MIB + 512, CopyScope::Everything);
        assert_eq!(plan.chunk(0), 0..MIB);
        assert_eq!(plan.chunk(2), 2 * MIB..2 * MIB + 512);
    }

    #[test]
    fn moving_right_copies_the_last_chunk_first() {
        let plan = plan(9 * MIB, 10 * MIB, 2 * MIB + 512, CopyScope::Everything);
        assert_eq!(plan.chunk(0), MIB + 512..2 * MIB + 512);
        assert_eq!(plan.chunk(2), 0..512);
    }

    #[test]
    fn refuses_moves_that_go_nowhere_or_are_unaligned() {
        let job = |source, destination, len| MoveJob {
            source,
            destination,
            len,
            scope: CopyScope::Everything,
        };
        assert!(ChunkPlan::new(job(MIB, MIB, MIB), 512, MAX_CHUNK).is_err());
        assert!(ChunkPlan::new(job(MIB, 0, 0), 512, MAX_CHUNK).is_err());
        assert!(ChunkPlan::new(job(MIB + 1, 0, MIB), 512, MAX_CHUNK).is_err());
    }

    #[test]
    fn pieces_are_block_aligned_and_clipped_to_the_chunk() {
        let scope = CopyScope::UsedOnly(vec![100..700, 3000..5000]);
        let plan = plan(8192, 0, 8192, scope);
        assert_eq!(plan.chunk_len(), 8192);
        assert_eq!(plan.pieces(0), vec![0..1024, 2560..5120]);
    }

    #[test]
    fn chunk_without_data_has_no_pieces() {
        let scope = CopyScope::UsedOnly(vec![0..512, 8192..8704]);
        let plan = plan(4096, 0, 4096 * 3, scope);
        assert!(plan.pieces(1).is_empty());
        assert_eq!(plan.pieces(2), vec![8192..8704]);
    }
}
