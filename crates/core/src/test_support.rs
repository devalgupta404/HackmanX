//! Builders for test layouts. Positions and sizes are in MiB on a 512-byte-sector disk.

use crate::layout::{Content, Extent, Geometry, Guid, Layout, Partition, Target};
use crate::units::{Lba, Sectors};

const SECTORS_PER_MIB: u64 = 2048;

pub fn mib(n: u64) -> Sectors {
    Sectors(n * SECTORS_PER_MIB)
}

pub fn at(n: u64) -> Lba {
    Lba(n * SECTORS_PER_MIB)
}

pub fn guid(number: u32) -> Guid {
    Guid([number as u8; 16])
}

/// A partition whose data fills half of it and that can shrink down to that half.
pub fn partition(number: u32, content: Content, start_mib: u64, len_mib: u64) -> Partition {
    Partition {
        number,
        unique_guid: guid(number),
        type_guid: Guid([0; 16]),
        name: String::new(),
        attributes: 0,
        extent: Extent::new(at(start_mib), mib(len_mib)),
        content,
        used: Sectors(mib(len_mib).0 / 2),
        min_len: Some(Sectors(mib(len_mib).0 / 2)),
    }
}

/// A disk of `total_mib`, usable from 1 MiB to 1 MiB before the end.
pub fn layout(total_mib: u64, partitions: Vec<Partition>) -> Layout {
    Layout {
        geometry: Geometry {
            sector_size: 512,
            usable: Extent::new(at(1), mib(total_mib - 2)),
            alignment: mib(1),
        },
        partitions,
    }
}

pub fn unchanged(layout: &Layout) -> Target {
    layout
        .partitions
        .iter()
        .map(|p| (p.unique_guid, p.extent))
        .collect()
}

/// The current layout with the listed partitions placed at `(number, start_mib, len_mib)`.
pub fn target_of(layout: &Layout, placements: &[(u32, u64, u64)]) -> Target {
    let mut target = unchanged(layout);
    for &(number, start_mib, len_mib) in placements {
        target.insert(guid(number), Extent::new(at(start_mib), mib(len_mib)));
    }
    target
}
