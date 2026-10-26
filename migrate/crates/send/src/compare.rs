//! The compare pass (architecture A5.2): the always-available capture backend, and the recovery
//! path whenever a tracker breaks (M4) or a rebooted source rejoins (U2). It reads the source in
//! fixed chunks, hashes each, and emits only the chunks whose hash differs from the receiver's
//! index — so the network carries just the differences, with no tracker at all. Run after a freeze,
//! the final compare pass is the exact delta and the proof input for the receipt.

use bootshift_disk::{BlockDevice, DiskError};
use bootshift_migrate_core::DiskRange;

/// A chunk of the source, hashed over the bytes exactly as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceChunk {
    pub source: DiskRange,
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

/// Compares the source, chunk by chunk, with the receiver's index (`target_hashes[i]` is the hash
/// of target chunk `i`, or zeros if the receiver has never written it), and returns the chunks
/// that differ. `chunk_size` must match the receiver's index granularity.
pub fn compare_pass(
    source: &dyn BlockDevice,
    target_hashes: &[[u8; 32]],
    chunk_size: u64,
) -> Result<Vec<SourceChunk>, DiskError> {
    let chunks = source.len_bytes().div_ceil(chunk_size.max(1));
    let mut diffs = Vec::new();
    for index in 0..chunks {
        let start = index * chunk_size;
        let end = ((index + 1) * chunk_size).min(source.len_bytes());
        let mut data = vec![0u8; (end - start) as usize];
        source.read_at(start, &mut data)?;
        let hash = *blake3::hash(&data).as_bytes();
        let known = target_hashes
            .get(index as usize)
            .copied()
            .unwrap_or([0u8; 32]);
        if hash != known {
            diffs.push(SourceChunk {
                source: DiskRange::new(start, end),
                hash,
                data,
            });
        }
    }
    Ok(diffs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootshift_disk::MemDisk;

    const CHUNK: u64 = 1 << 20;

    fn disk(fill: u8) -> MemDisk {
        MemDisk::from_bytes(vec![fill; (4 * CHUNK) as usize], 4096).expect("disk")
    }

    #[test]
    fn with_an_empty_index_every_chunk_differs() {
        let source = disk(9);
        let diffs = compare_pass(&source, &[], CHUNK).expect("compare");
        assert_eq!(diffs.len(), 4);
        assert_eq!(diffs[0].source, DiskRange::new(0, CHUNK));
    }

    #[test]
    fn a_matching_index_yields_no_differences() {
        let source = disk(9);
        // Hash each chunk to build a matching index.
        let hashes: Vec<[u8; 32]> = (0..4)
            .map(|i| {
                let mut buf = vec![0u8; CHUNK as usize];
                source.read_at(i * CHUNK, &mut buf).expect("read");
                *blake3::hash(&buf).as_bytes()
            })
            .collect();
        assert!(
            compare_pass(&source, &hashes, CHUNK)
                .expect("compare")
                .is_empty()
        );
    }

    #[test]
    fn only_the_changed_chunk_is_sent() {
        let mut source = disk(9);
        let hashes: Vec<[u8; 32]> = (0..4)
            .map(|i| {
                let mut buf = vec![0u8; CHUNK as usize];
                source.read_at(i * CHUNK, &mut buf).expect("read");
                *blake3::hash(&buf).as_bytes()
            })
            .collect();
        // Change the third chunk only.
        source
            .write_at(2 * CHUNK, &vec![1u8; CHUNK as usize])
            .expect("write");
        let diffs = compare_pass(&source, &hashes, CHUNK).expect("compare");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].source, DiskRange::new(2 * CHUNK, 3 * CHUNK));
    }
}
