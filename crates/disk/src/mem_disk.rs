use crate::device::{BlockDevice, DiskError, check_geometry, checked_range};

/// An in-memory disk where every write is durable immediately.
#[derive(Clone, PartialEq, Eq)]
pub struct MemDisk {
    bytes: Vec<u8>,
    block_size: u32,
}

impl MemDisk {
    pub fn zeroed(len_bytes: usize, block_size: u32) -> Result<Self, DiskError> {
        Self::from_bytes(vec![0; len_bytes], block_size)
    }

    pub fn from_bytes(bytes: Vec<u8>, block_size: u32) -> Result<Self, DiskError> {
        check_geometry(bytes.len() as u64, block_size)?;
        Ok(Self { bytes, block_size })
    }

    pub(crate) fn from_checked_parts(bytes: Vec<u8>, block_size: u32) -> Self {
        Self { bytes, block_size }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl BlockDevice for MemDisk {
    fn len_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn logical_block_size(&self) -> u32 {
        self.block_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        let range = checked_range(offset, buf.len(), self.len_bytes(), self.block_size)?;
        buf.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        let range = checked_range(offset, data.len(), self.len_bytes(), self.block_size)?;
        self.bytes[range].copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_was_written() {
        let mut disk = MemDisk::zeroed(4096, 512).unwrap();
        disk.write_at(1024, &[7; 512]).unwrap();

        let mut buf = [0; 1024];
        disk.read_at(512, &mut buf).unwrap();

        assert_eq!(&buf[..512], &[0; 512]);
        assert_eq!(&buf[512..], &[7; 512]);
    }

    #[test]
    fn rejects_invalid_geometry() {
        assert!(MemDisk::zeroed(1000, 512).is_err());
    }

    #[test]
    fn rejects_misaligned_write_without_changing_anything() {
        let mut disk = MemDisk::zeroed(4096, 512).unwrap();
        assert!(disk.write_at(10, &[1; 512]).is_err());
        assert!(disk.bytes().iter().all(|&b| b == 0));
    }
}
