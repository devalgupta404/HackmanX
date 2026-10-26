//! Where a file's bytes sit on its block device, through the classic FIBMAP ioctl.
//!
//! FIEMAP would be nicer, but vfat (every EFI system partition) answers only FIBMAP, and
//! `filefrag -v` silently prints no extent table there. FIBMAP needs root, which everything
//! that locates the journal already has.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::FileExtent;

const FIBMAP: libc::c_ulong = 1;
const FIGETBSZ: libc::c_ulong = 2;

/// The file's extents as byte offsets within its filesystem, in file order, coalesced.
pub fn file_extents(path: &Path) -> io::Result<Vec<FileExtent>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let block_size = block_size(&file)?;
    let mut extents: Vec<FileExtent> = Vec::new();
    for index in 0..len.div_ceil(block_size) {
        let physical = physical_block(&file, index)?;
        if physical == 0 {
            return Err(io::Error::other(format!(
                "block {index} of {} has no place on disk",
                path.display()
            )));
        }
        let device_offset = physical * block_size;
        match extents.last_mut() {
            Some(last) if last.device_offset + last.len == device_offset => {
                last.len += block_size;
            }
            _ => extents.push(FileExtent {
                file_offset: index * block_size,
                device_offset,
                len: block_size,
            }),
        }
    }
    if extents.is_empty() {
        return Err(io::Error::other(format!("{} is empty", path.display())));
    }
    Ok(extents)
}

#[allow(unsafe_code)]
fn block_size(file: &File) -> io::Result<u64> {
    let mut size: libc::c_int = 0;
    // SAFETY: FIGETBSZ writes one c_int through the pointer, which outlives the call.
    let code = unsafe { libc::ioctl(file.as_raw_fd(), FIGETBSZ as _, &mut size) };
    if code != 0 || size <= 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(size as u64)
}

#[allow(unsafe_code)]
fn physical_block(file: &File, index: u64) -> io::Result<u64> {
    let mut block = libc::c_int::try_from(index)
        .map_err(|_| io::Error::other("the file has too many blocks for FIBMAP"))?;
    // SAFETY: FIBMAP reads and writes one c_int through the pointer, which outlives the call.
    let code = unsafe { libc::ioctl(file.as_raw_fd(), FIBMAP as _, &mut block) };
    if code != 0 {
        return Err(io::Error::last_os_error());
    }
    u64::try_from(block).map_err(|_| io::Error::other("FIBMAP returned a negative block"))
}
