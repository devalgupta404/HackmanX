use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::OFlags;

use crate::device::{BlockDevice, DiskError, check_access, check_geometry};

const BOUNCE_LEN: usize = 8 << 20;
const BOUNCE_ALIGN: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    /// Exclusive on block devices: the kernel refuses the open while any partition of the
    /// device is mounted or otherwise in use.
    ReadWrite,
    /// Not exclusive, for the recovery system: nothing is mounted there, and the filesystem
    /// tools must still be able to open the partitions while BootShift holds the disk.
    ReadWriteShared,
}

/// A disk or disk image opened with `O_DIRECT`, so reads and writes bypass the page cache.
///
/// `O_DIRECT` needs buffers aligned in memory, so data passes through an aligned bounce buffer.
/// Writes are durable only after `flush`, which calls `fdatasync` and makes the kernel flush
/// the drive's write cache.
pub struct RealDevice {
    file: File,
    path: PathBuf,
    len: u64,
    block_size: u32,
    bounce: AlignedBuffer,
}

impl RealDevice {
    /// Opens a block device, taking its logical block size from the kernel.
    pub fn open(path: &Path, access: Access) -> Result<Self, DiskError> {
        let open_error = |source| DiskError::Open {
            path: path.to_path_buf(),
            source,
        };
        let is_block_device = std::fs::metadata(path)
            .map_err(open_error)?
            .file_type()
            .is_block_device();
        if !is_block_device {
            return Err(open_error(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a block device",
            )));
        }
        let file = open_direct(path, access, true).map_err(open_error)?;
        let block_size =
            rustix::fs::ioctl_blksszget(&file).map_err(|errno| open_error(errno.into()))?;
        Self::from_file(file, path, block_size)
    }

    /// Opens a disk image file, treating it as a disk with `block_size`-byte blocks.
    pub fn open_image(path: &Path, access: Access, block_size: u32) -> Result<Self, DiskError> {
        let file = open_direct(path, access, false).map_err(|source| DiskError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_file(file, path, block_size)
    }

    fn from_file(mut file: File, path: &Path, block_size: u32) -> Result<Self, DiskError> {
        let len = file
            .seek(SeekFrom::End(0))
            .map_err(|source| DiskError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        check_geometry(len, block_size)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            len,
            block_size,
            bounce: AlignedBuffer::new(BOUNCE_LEN),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BlockDevice for RealDevice {
    fn len_bytes(&self) -> u64 {
        self.len
    }

    fn logical_block_size(&self) -> u32 {
        self.block_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        check_access(offset, buf.len(), self.len, self.block_size)?;
        let mut bounce = AlignedBuffer::new(buf.len().min(BOUNCE_LEN));
        for (index, piece) in buf.chunks_mut(BOUNCE_LEN).enumerate() {
            let piece_offset = offset + (index * BOUNCE_LEN) as u64;
            let aligned = bounce.slice(piece.len());
            self.file
                .read_exact_at(aligned, piece_offset)
                .map_err(|source| io_error(piece_offset, source))?;
            piece.copy_from_slice(aligned);
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError> {
        check_access(offset, data.len(), self.len, self.block_size)?;
        for (index, piece) in data.chunks(BOUNCE_LEN).enumerate() {
            let piece_offset = offset + (index * BOUNCE_LEN) as u64;
            let aligned = self.bounce.slice(piece.len());
            aligned.copy_from_slice(piece);
            self.file
                .write_all_at(aligned, piece_offset)
                .map_err(|source| io_error(piece_offset, source))?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.file.sync_data().map_err(|source| io_error(0, source))
    }
}

fn open_direct(path: &Path, access: Access, exclusive: bool) -> io::Result<File> {
    let mut flags = OFlags::DIRECT;
    if exclusive && access == Access::ReadWrite {
        flags |= OFlags::EXCL;
    }
    OpenOptions::new()
        .read(true)
        .write(access != Access::ReadOnly)
        .custom_flags(flags.bits() as i32)
        .open(path)
}

fn io_error(offset: u64, source: io::Error) -> DiskError {
    DiskError::Io { offset, source }
}

/// Heap memory whose start is aligned for `O_DIRECT`, carved out of a slightly larger `Vec`.
struct AlignedBuffer {
    storage: Vec<u8>,
    start: usize,
}

impl AlignedBuffer {
    fn new(len: usize) -> Self {
        let storage = vec![0; len + BOUNCE_ALIGN];
        let start = storage.as_ptr().align_offset(BOUNCE_ALIGN);
        Self { storage, start }
    }

    fn slice(&mut self, len: usize) -> &mut [u8] {
        &mut self.storage[self.start..self.start + len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buffer_starts_on_a_4_kib_boundary() {
        let mut buffer = AlignedBuffer::new(10_000);
        let slice = buffer.slice(10_000);
        assert_eq!(slice.as_ptr() as usize % BOUNCE_ALIGN, 0);
        assert_eq!(slice.len(), 10_000);
    }
}
