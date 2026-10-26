use std::iter::Sum;
use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// A position on the disk, in logical sectors from the start.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Lba(pub u64);

/// A length on the disk, in logical sectors.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Sectors(pub u64);

impl Lba {
    pub fn distance(self, other: Lba) -> Sectors {
        Sectors(self.0.abs_diff(other.0))
    }

    pub fn is_aligned(self, alignment: Sectors) -> bool {
        self.0.checked_rem(alignment.0) == Some(0)
    }

    pub fn align_up(self, alignment: Sectors) -> Lba {
        match self.0.checked_rem(alignment.0) {
            Some(0) | None => self,
            Some(rem) => Lba(self.0.saturating_add(alignment.0 - rem)),
        }
    }
}

impl Sectors {
    pub fn to_bytes(self, sector_size: u32) -> u64 {
        self.0.saturating_mul(u64::from(sector_size))
    }
}

/// A byte count the way people read it, such as `476.9 GiB`. Shared by the CLI and the GUI, so
/// both say the same thing about the same disk.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// Saturating, so absurd input (such as an untrusted plan) fails a bounds check instead of
// wrapping around to a small, valid-looking position.
impl Add<Sectors> for Lba {
    type Output = Lba;

    fn add(self, len: Sectors) -> Lba {
        Lba(self.0.saturating_add(len.0))
    }
}

impl Add for Sectors {
    type Output = Sectors;

    fn add(self, other: Sectors) -> Sectors {
        Sectors(self.0.saturating_add(other.0))
    }
}

impl AddAssign for Sectors {
    fn add_assign(&mut self, other: Sectors) {
        *self = *self + other;
    }
}

impl Sum for Sectors {
    fn sum<I: Iterator<Item = Sectors>>(iter: I) -> Sectors {
        iter.fold(Sectors(0), Add::add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_past_the_end_saturates_instead_of_wrapping() {
        assert_eq!(Lba(u64::MAX - 1) + Sectors(10), Lba(u64::MAX));
    }

    #[test]
    fn distance_is_the_same_in_both_directions() {
        assert_eq!(Lba(10).distance(Lba(4)), Sectors(6));
        assert_eq!(Lba(4).distance(Lba(10)), Sectors(6));
    }

    #[test]
    fn alignment_to_zero_is_never_satisfied() {
        assert!(!Lba(0).is_aligned(Sectors(0)));
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(256 << 20), "256.0 MiB");
        assert_eq!(format_bytes(512_110_190_592), "476.9 GiB");
    }

    #[test]
    fn align_up_rounds_to_the_next_multiple() {
        assert_eq!(Lba(2049).align_up(Sectors(2048)), Lba(4096));
        assert_eq!(Lba(4096).align_up(Sectors(2048)), Lba(4096));
    }
}
