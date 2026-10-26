//! Turning sectors and counts into the words and numbers the screens show.

use bootshift_core::layout::{Layout, Partition};
use bootshift_core::units::{Sectors, format_bytes};

pub fn size(layout: &Layout, sectors: Sectors) -> String {
    format_bytes(sectors.to_bytes(layout.geometry.sector_size))
}

/// What the bar, the capsules and the table call a partition: its GPT name, or its contents.
pub fn short_name(partition: &Partition) -> String {
    if partition.name.is_empty() {
        partition.content.name().to_string()
    } else {
        partition.name.clone()
    }
}

/// What the dimension lines call a partition that moves.
pub fn mover_name(partition: &Partition) -> String {
    if partition.name.is_empty() {
        format!("partition {}", partition.number)
    } else {
        partition.name.clone()
    }
}

/// `421636096` as `421,636,096`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A rough copy time as `Nm Ss`, never less than a second.
pub fn rough_time(bytes: u64, bytes_per_second: u64) -> String {
    let seconds = (bytes as f64 / bytes_per_second as f64).round().max(1.0) as u64;
    format!("{}m {}s", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_numbers_get_thousand_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(421_636_096), "421,636,096");
    }

    #[test]
    fn rough_time_rounds_to_whole_seconds_and_never_says_zero() {
        let speed = 450 << 20;
        assert_eq!(rough_time(0, speed), "0m 1s");
        assert_eq!(rough_time(90 * (1 << 30), speed), "3m 25s");
    }
}
