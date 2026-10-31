use std::ops::Range;

/// A disk that Kitten reads and writes. Every byte the engine changes goes through this trait.
///
/// Offsets and lengths must be multiples of [`BlockDevice::logical_block_size`], the same rule
/// `O_DIRECT` enforces on real devices, so in-memory test disks catch misaligned I/O too.
/// A write is only durable after [`BlockDevice::flush`] returns.
pub trait BlockDevice {
    fn len_bytes(&self) -> u64;
    fn logical_block_size(&self) -> u32;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiskError>;
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), DiskError>;
    fn flush(&mut self) -> Result<(), DiskError>;
}

#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    #[error("{len_bytes}-byte disk with {block_size}-byte blocks is not a valid geometry")]
    InvalidGeometry { len_bytes: u64, block_size: u32 },

    #[error("{len} bytes at offset {offset} is not aligned to the {block_size}-byte block size")]
    Misaligned {
        offset: u64,
        len: u64,
        block_size: u32,
    },

    #[error("{len} bytes at offset {offset} runs past the end of the {device_len}-byte disk")]
    OutOfBounds {
        offset: u64,
        len: u64,
        device_len: u64,
    },

    #[error("I/O error at byte {offset}")]
    Io {
        offset: u64,
        #[source]
        source: std::io::Error,
    },

    #[error("can't open {path}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) fn check_geometry(len_bytes: u64, block_size: u32) -> Result<(), DiskError> {
    let valid =
        block_size >= 512 && block_size.is_power_of_two() && len_bytes % u64::from(block_size) == 0;
    if valid {
        Ok(())
    } else {
        Err(DiskError::InvalidGeometry {
            len_bytes,
            block_size,
        })
    }
}

/// Checks alignment and bounds, and returns the end offset of the access.
pub(crate) fn check_access(
    offset: u64,
    len: usize,
    device_len: u64,
    block_size: u32,
) -> Result<u64, DiskError> {
    let len = len as u64;
    let block = u64::from(block_size);
    if offset % block != 0 || len % block != 0 {
        return Err(DiskError::Misaligned {
            offset,
            len,
            block_size,
        });
    }
    match offset.checked_add(len) {
        Some(end) if end <= device_len => Ok(end),
        _ => Err(DiskError::OutOfBounds {
            offset,
            len,
            device_len,
        }),
    }
}

/// Checks alignment and bounds, and returns the byte range to touch in an in-memory buffer.
pub(crate) fn checked_range(
    offset: u64,
    len: usize,
    device_len: u64,
    block_size: u32,
) -> Result<Range<usize>, DiskError> {
    let end = check_access(offset, len, device_len, block_size)?;
    Ok(offset as usize..end as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_requires_power_of_two_block_of_at_least_512_bytes() {
        assert!(check_geometry(4096, 512).is_ok());
        assert!(check_geometry(8192, 4096).is_ok());
        assert!(check_geometry(4096, 256).is_err());
        assert!(check_geometry(6144, 3072).is_err());
    }

    #[test]
    fn geometry_requires_length_to_be_whole_blocks() {
        assert!(check_geometry(1000, 512).is_err());
    }

    #[test]
    fn range_rejects_unaligned_offset_or_length() {
        assert!(matches!(
            checked_range(100, 512, 4096, 512),
            Err(DiskError::Misaligned { .. })
        ));
        assert!(matches!(
            checked_range(512, 100, 4096, 512),
            Err(DiskError::Misaligned { .. })
        ));
    }

    #[test]
    fn range_rejects_access_past_the_end() {
        assert!(matches!(
            checked_range(4096, 512, 4096, 512),
            Err(DiskError::OutOfBounds { .. })
        ));
        assert!(matches!(
            checked_range(u64::MAX - 511, 512, 4096, 512),
            Err(DiskError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn range_accepts_access_ending_exactly_at_the_end() {
        assert_eq!(checked_range(3584, 512, 4096, 512).unwrap(), 3584..4096);
    }
}
