use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};

use crate::units::{Lba, Sectors};

/// A GPT GUID in its on-disk byte order (the first three fields are little-endian).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, SerializeDisplay, DeserializeFromStr,
)]
pub struct Guid(pub [u8; 16]);

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9]
        )?;
        b[10..].iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl Guid {
    /// The partition type of an EFI system partition.
    pub const EFI_SYSTEM: Guid = Guid([
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ]);
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("\"{0}\" is not a GUID")]
pub struct GuidParseError(String);

impl FromStr for Guid {
    type Err = GuidParseError;

    /// Parses the canonical `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form, in either case.
    fn from_str(text: &str) -> Result<Guid, GuidParseError> {
        let error = || GuidParseError(text.to_string());
        let groups: Vec<&str> = text.split('-').collect();
        let lengths: Vec<usize> = groups.iter().map(|g| g.len()).collect();
        if lengths != [8, 4, 4, 4, 12] {
            return Err(error());
        }
        let hex: String = groups.concat();
        let mut canonical = [0u8; 16];
        for (index, byte) in canonical.iter_mut().enumerate() {
            let pair = hex.get(index * 2..index * 2 + 2).ok_or_else(error)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| error())?;
        }
        let c = canonical;
        Ok(Guid([
            c[3], c[2], c[1], c[0], c[5], c[4], c[7], c[6], c[8], c[9], c[10], c[11], c[12], c[13],
            c[14], c[15],
        ]))
    }
}

/// A contiguous run of sectors from `start` up to, but not including, `end()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    pub start: Lba,
    pub len: Sectors,
}

impl Extent {
    pub fn new(start: Lba, len: Sectors) -> Self {
        Self { start, len }
    }

    pub fn end(self) -> Lba {
        self.start + self.len
    }

    pub fn overlaps(self, other: Extent) -> bool {
        self.start < other.end() && other.start < self.end()
    }

    pub fn contains(self, other: Extent) -> bool {
        other.start >= self.start && other.end() <= self.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub sector_size: u32,
    /// The sectors partitions may occupy, between the primary and backup GPT.
    pub usable: Extent,
    /// Moved starts and resized ends land on multiples of this (1 MiB).
    pub alignment: Sectors,
}

/// What a partition holds, which decides what BootShift may do with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Content {
    EfiSystem,
    Ext4,
    Ntfs,
    Btrfs,
    Xfs,
    Swap,
    BitLocker,
    /// Anything BootShift can't read inside (LVM, LUKS, FAT data, unknown). Moved byte for byte.
    Opaque,
}

impl Content {
    pub fn name(self) -> &'static str {
        match self {
            Content::EfiSystem => "EFI system",
            Content::Ext4 => "ext4",
            Content::Ntfs => "NTFS",
            Content::Btrfs => "Btrfs",
            Content::Xfs => "XFS",
            Content::Swap => "swap",
            Content::BitLocker => "BitLocker",
            Content::Opaque => "unrecognised",
        }
    }

    pub fn can_move(self) -> bool {
        !matches!(self, Content::EfiSystem | Content::BitLocker)
    }

    pub fn can_grow(self) -> bool {
        matches!(
            self,
            Content::Ext4 | Content::Ntfs | Content::Btrfs | Content::Xfs | Content::Swap
        )
    }

    pub fn can_shrink(self) -> bool {
        matches!(
            self,
            Content::Ext4 | Content::Ntfs | Content::Btrfs | Content::Swap
        )
    }

    /// Whether a move can skip free space, because we can read this filesystem's used-space map.
    pub fn copies_used_only(self) -> bool {
        matches!(
            self,
            Content::Ext4 | Content::Ntfs | Content::Btrfs | Content::Xfs
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    /// The GPT entry number, as shown to the user (1-based).
    pub number: u32,
    pub unique_guid: Guid,
    pub type_guid: Guid,
    pub name: String,
    pub attributes: u64,
    pub extent: Extent,
    pub content: Content,
    pub used: Sectors,
    /// The smallest size the filesystem can shrink to, if it could be measured.
    pub min_len: Option<Sectors>,
}

impl Partition {
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("partition {} ({})", self.number, self.content.name())
        } else {
            format!("partition {} \"{}\"", self.number, self.name)
        }
    }

    pub fn sectors_to_copy(&self) -> Sectors {
        if self.content.copies_used_only() {
            self.used.min(self.extent.len)
        } else {
            self.extent.len
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub geometry: Geometry,
    pub partitions: Vec<Partition>,
}

impl Layout {
    pub fn partition(&self, guid: Guid) -> Option<&Partition> {
        self.partitions.iter().find(|p| p.unique_guid == guid)
    }

    pub(crate) fn partition_mut(&mut self, guid: Guid) -> Option<&mut Partition> {
        self.partitions.iter_mut().find(|p| p.unique_guid == guid)
    }
}

/// Where the user wants each partition, keyed by its unique GUID.
pub type Target = BTreeMap<Guid, Extent>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_displays_in_canonical_form() {
        let esp = Guid([
            0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
            0xc9, 0x3b,
        ]);
        assert_eq!(esp.to_string(), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
        assert_eq!(esp, Guid::EFI_SYSTEM);
    }

    #[test]
    fn guid_parses_back_from_its_canonical_form() {
        let text = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
        let guid: Guid = text.parse().unwrap();
        assert_eq!(guid.to_string(), text.to_lowercase());
    }

    #[test]
    fn malformed_guids_are_refused() {
        for text in [
            "",
            "c12a7328f81f11d2ba4b00a0c93ec93b",
            "c12a7328-f81f-11d2-ba4b-00a0c93ec93",
            "z12a7328-f81f-11d2-ba4b-00a0c93ec93b",
        ] {
            assert!(text.parse::<Guid>().is_err(), "{text}");
        }
    }

    #[test]
    fn touching_extents_do_not_overlap() {
        let a = Extent::new(Lba(0), Sectors(10));
        let b = Extent::new(Lba(10), Sectors(10));
        assert!(!a.overlaps(b));
        assert!(a.overlaps(Extent::new(Lba(9), Sectors(1))));
    }

    #[test]
    fn extent_contains_itself_but_not_a_larger_one() {
        let a = Extent::new(Lba(10), Sectors(10));
        assert!(a.contains(a));
        assert!(!a.contains(Extent::new(Lba(10), Sectors(11))));
    }
}
