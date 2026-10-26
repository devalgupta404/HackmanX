//! NTFS through ntfs-3g's `ntfsresize` and `ntfscat`.

use std::ops::Range;
use std::path::Path;

use crate::parse::{number_field, unexpected};
use crate::run::{CommandLine, RunError, Runner};

/// `$Bitmap` is the file with MFT record number 6; one bit per cluster, set when used.
const BITMAP_RECORD: &str = "6";

/// Whether the volume may carry its "needs checking" mark.
///
/// `ntfsresize` always sets the mark, so after BootShift's own resize it is expected. On a
/// volume as found, it means Windows wasn't shut down cleanly, and the volume is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirtyMark {
    Refuse,
    ExpectedAfterOurResize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NtfsInfo {
    pub cluster_size: u64,
    /// The filesystem's size. The backup boot sector sits just past it, in the partition's
    /// last sector.
    pub volume_size: u64,
    /// The smallest size `ntfsresize` can shrink to, if it can shrink at all.
    pub min_size: Option<u64>,
}

/// Checks the volume and reads its geometry without changing anything. Fails, with
/// `ntfsresize`'s explanation, if Windows is hibernated, the volume is marked for checking,
/// or anything else makes resizing unsafe.
pub fn info(runner: &dyn Runner, device: &Path, dirty: DirtyMark) -> Result<NtfsInfo, RunError> {
    let command = with_force(
        CommandLine::new("ntfsresize")
            .arg("--info")
            .arg("--no-action")
            .arg("--no-progress-bar"),
        dirty,
    )
    .path(device);
    let output = runner.run_ok(&command)?;
    parse_info(&output.text()).map_err(|detail| unexpected(&command, detail))
}

/// Byte ranges of used clusters, relative to the start of the filesystem.
pub fn used_ranges(
    runner: &dyn Runner,
    device: &Path,
    info: &NtfsInfo,
    dirty: DirtyMark,
) -> Result<Vec<Range<u64>>, RunError> {
    let command = with_force(CommandLine::new("ntfscat"), dirty)
        .arg("-i")
        .arg(BITMAP_RECORD)
        .path(device);
    let output = runner.run_ok(&command)?;
    parse_bitmap(&output.stdout, info).map_err(|detail| unexpected(&command, detail))
}

/// Resizes the filesystem to `size` bytes, or to fill its partition when `size` is `None`.
/// Never call this on a real partition outside a guarded run: `ntfsresize` can't survive a
/// power cut on its own. Windows runs chkdsk once on its next boot afterwards.
pub fn resize_offline(
    runner: &dyn Runner,
    device: &Path,
    size: Option<u64>,
) -> Result<(), RunError> {
    let mut command = CommandLine::new("ntfsresize")
        .arg("--force")
        .arg("--no-progress-bar");
    if let Some(size) = size {
        command = command.arg("--size").arg(size.to_string());
    }
    runner.run_ok(&command.path(device)).map(|_| ())
}

fn with_force(command: CommandLine, dirty: DirtyMark) -> CommandLine {
    match dirty {
        DirtyMark::Refuse => command,
        DirtyMark::ExpectedAfterOurResize => command.arg("--force"),
    }
}

pub fn parse_info(text: &str) -> Result<NtfsInfo, String> {
    let min_size = text.lines().find_map(|line| {
        line.strip_prefix("You might resize at ")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    });
    Ok(NtfsInfo {
        cluster_size: number_field(text, "Cluster size")?,
        volume_size: number_field(text, "Current volume size")?,
        min_size,
    })
}

pub fn parse_bitmap(bitmap: &[u8], info: &NtfsInfo) -> Result<Vec<Range<u64>>, String> {
    if info.cluster_size == 0 {
        return Err("cluster size is zero".to_string());
    }
    let clusters = info.volume_size / info.cluster_size;
    if (bitmap.len() as u64) * 8 < clusters {
        return Err(format!(
            "$Bitmap covers {} clusters but the volume has {clusters}",
            bitmap.len() * 8
        ));
    }

    let mut used: Vec<Range<u64>> = Vec::new();
    for cluster in (0..clusters).filter(|&c| bitmap[(c / 8) as usize] & (1 << (c % 8)) != 0) {
        let start = cluster * info.cluster_size;
        match used.last_mut() {
            Some(run) if run.end == start => run.end += info.cluster_size,
            _ => used.push(start..start + info.cluster_size),
        }
    }
    Ok(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = include_str!("../tests/fixtures/ntfs/ntfsresize-info.txt");
    const BITMAP: &[u8] = include_bytes!("../tests/fixtures/ntfs/bitmap.bin");

    fn info_of(cluster_size: u64, clusters: u64) -> NtfsInfo {
        NtfsInfo {
            cluster_size,
            volume_size: cluster_size * clusters,
            min_size: None,
        }
    }

    #[test]
    fn parses_ntfsresize_info() {
        assert_eq!(
            parse_info(INFO).unwrap(),
            NtfsInfo {
                cluster_size: 4096,
                volume_size: 67_105_280,
                min_size: Some(2_564_096),
            }
        );
    }

    #[test]
    fn bitmap_bits_become_merged_cluster_ranges() {
        let bitmap = [0b0000_0111, 0b1000_0000];
        assert_eq!(
            parse_bitmap(&bitmap, &info_of(4096, 16)).unwrap(),
            vec![0..3 * 4096, 15 * 4096..16 * 4096]
        );
    }

    #[test]
    fn bits_past_the_last_cluster_are_ignored() {
        assert_eq!(
            parse_bitmap(&[0xFF], &info_of(512, 3)).unwrap(),
            vec![0..3 * 512]
        );
    }

    #[test]
    fn refuses_a_bitmap_shorter_than_the_volume() {
        assert!(parse_bitmap(&[0xFF], &info_of(4096, 9)).is_err());
    }

    #[test]
    fn real_bitmap_starts_with_the_boot_area_in_use() {
        let info = parse_info(INFO).unwrap();
        let used = parse_bitmap(BITMAP, &info).unwrap();
        assert_eq!(used.first().map(|r| r.start), Some(0));
        let used_bytes: u64 = used.iter().map(|r| r.end - r.start).sum();
        assert!(used_bytes < info.volume_size / 10);
    }
}
