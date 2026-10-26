//! NTFS keeps its partition's start sector in its boot sector ("hidden sectors", offset 0x1C),
//! and a backup boot sector in the partition's last sector. After a move both must point at
//! the new start, or Windows' boot code looks in the wrong place.

use bootshift_core::layout::Extent;
use bootshift_disk::{BlockDevice, DiskError};

const OEM_ID: &[u8; 8] = b"NTFS    ";
const OEM_ID_OFFSET: usize = 3;
const HIDDEN_SECTORS_OFFSET: usize = 0x1C;

/// Sets the hidden-sectors field of both boot sectors of the NTFS volume in `extent`. Sectors
/// without the NTFS signature are left alone, and so is a start beyond what the 32-bit field
/// holds (UEFI booting doesn't read it). Safe to repeat.
pub fn fix_hidden_sectors<D: BlockDevice + ?Sized>(
    disk: &mut D,
    extent: Extent,
) -> Result<(), DiskError> {
    let Ok(start) = u32::try_from(extent.start.0) else {
        return Ok(());
    };
    let sector_size = u64::from(disk.logical_block_size());
    let first = extent.start.0 * sector_size;
    let last = (extent.end().0 - 1) * sector_size;
    for offset in [first, last] {
        let mut sector = vec![0; sector_size as usize];
        disk.read_at(offset, &mut sector)?;
        if &sector[OEM_ID_OFFSET..OEM_ID_OFFSET + OEM_ID.len()] == OEM_ID {
            sector[HIDDEN_SECTORS_OFFSET..HIDDEN_SECTORS_OFFSET + 4]
                .copy_from_slice(&start.to_le_bytes());
            disk.write_at(offset, &sector)?;
        }
    }
    disk.flush()
}

#[cfg(test)]
mod tests {
    use bootshift_core::units::{Lba, Sectors};
    use bootshift_disk::MemDisk;

    use super::*;

    fn boot_sector(hidden: u32) -> Vec<u8> {
        let mut sector = vec![0; 512];
        sector[OEM_ID_OFFSET..OEM_ID_OFFSET + 8].copy_from_slice(OEM_ID);
        sector[HIDDEN_SECTORS_OFFSET..HIDDEN_SECTORS_OFFSET + 4]
            .copy_from_slice(&hidden.to_le_bytes());
        sector
    }

    fn hidden_sectors(disk: &MemDisk, sector: u64) -> u32 {
        let at = (sector * 512) as usize + HIDDEN_SECTORS_OFFSET;
        u32::from_le_bytes(disk.bytes()[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn updates_both_boot_sectors_to_the_new_start() {
        let mut disk = MemDisk::zeroed(64 * 512, 512).unwrap();
        disk.write_at(8 * 512, &boot_sector(2048)).unwrap();
        disk.write_at(23 * 512, &boot_sector(2048)).unwrap();

        fix_hidden_sectors(&mut disk, Extent::new(Lba(8), Sectors(16))).unwrap();

        assert_eq!(hidden_sectors(&disk, 8), 8);
        assert_eq!(hidden_sectors(&disk, 23), 8);
    }

    #[test]
    fn leaves_sectors_that_are_not_ntfs_alone() {
        let mut disk = MemDisk::zeroed(64 * 512, 512).unwrap();
        let before = disk.clone();

        fix_hidden_sectors(&mut disk, Extent::new(Lba(8), Sectors(16))).unwrap();

        assert!(disk == before);
    }
}
