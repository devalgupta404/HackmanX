use std::collections::BTreeSet;

use crate::device::{BlockDevice, DiskError};

const EIO: i32 = 5;

/// Wraps a disk so that reading chosen blocks fails with an I/O error, like a failing sector.
/// Writing a bad block makes it readable again, the way a drive remaps a sector on write.
pub struct FaultyDisk<D> {
    inner: D,
    bad_blocks: BTreeSet<u64>,
}

impl<D: BlockDevice> FaultyDisk<D> {
    /// `bad_blocks` are byte offsets of whole logical blocks.
    pub fn new(inner: D, bad_blocks: impl IntoIterator<Item = u64>) -> Self {
        Self {
            inner,
            bad_blocks: bad_blocks.into_iter().collect(),
        }
    }

    pub fn into_inner(self) -> D {
        self.inner
    }

    fn first_bad_block(&self, offset: u64, len: usize) -> Option<u64> {
        let end = offset.saturating_add(len as u64);
        self.bad_blocks.range(offset..end).next().copied()
    }
}

impl<D: BlockDevice> BlockDevice for FaultyDisk<D> {
    fn len_bytes(&self) -> u64 {
        self.inner.len_bytes()
    }

    fn logical_block_size(&self) -> u32 {
        self.inner.logical_block_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        if let Some(bad) = self.first_bad_block(offset, buf.len()) {
            return Err(DiskError::Io {
                offset: bad,
                source: std::io::Error::from_raw_os_error(EIO),
            });
        }
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        self.inner.write_at(offset, data)?;
        let end = offset.saturating_add(data.len() as u64);
        self.bad_blocks
            .retain(|block| !(offset..end).contains(block));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemDisk;

    #[test]
    fn reading_a_bad_block_fails_with_its_offset() {
        let disk = FaultyDisk::new(MemDisk::zeroed(4096, 512).unwrap(), [1024]);
        let mut buf = [0; 2048];

        let error = disk.read_at(0, &mut buf).unwrap_err();

        assert!(matches!(error, DiskError::Io { offset: 1024, .. }));
        assert!(disk.read_at(2048, &mut buf).is_ok());
    }

    #[test]
    fn writing_a_bad_block_makes_it_readable() {
        let mut disk = FaultyDisk::new(MemDisk::zeroed(4096, 512).unwrap(), [1024]);
        disk.write_at(1024, &[1; 512]).unwrap();

        let mut buf = [0; 512];
        disk.read_at(1024, &mut buf).unwrap();

        assert_eq!(buf, [1; 512]);
    }
}
