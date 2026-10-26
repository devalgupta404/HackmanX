//! Creating the journal file on the mounted ESP and finding where its bytes sit on the disk,
//! so the recovery system can write it raw without mounting the ESP.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use bootshift_disk::FileExtent;
use bootshift_fs::run::{CommandLine, RunError, Runner};

/// `filefrag` flags meaning the extent's place on disk isn't fixed or isn't plain data.
const UNSAFE_FLAGS: [&str; 5] = ["unknown", "delalloc", "encoded", "inline", "not_aligned"];
const UNIT: u64 = 512;

/// Writes the journal file and flushes it and its directory, so its extents are allocated and
/// its contents are on disk before they are located.
pub fn create(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// The file's extents as byte offsets on the whole disk, given where its filesystem's
/// partition starts.
pub fn extents(
    runner: &dyn Runner,
    path: &Path,
    partition_offset: u64,
) -> Result<Vec<FileExtent>, RunError> {
    let command = CommandLine::new("filefrag")
        .arg("-v")
        .arg(format!("-b{UNIT}"))
        .path(path);
    let output = runner.run_ok(&command)?;
    let extents = parse_filefrag(&output.text()).map_err(|detail| RunError::UnexpectedOutput {
        command: command.to_string(),
        detail,
    })?;
    Ok(extents
        .into_iter()
        .map(|extent| FileExtent {
            device_offset: partition_offset + extent.device_offset,
            ..extent
        })
        .collect())
}

/// Parses `filefrag -v -b512`, returning byte offsets within the file and its filesystem.
pub fn parse_filefrag(text: &str) -> Result<Vec<FileExtent>, String> {
    let mut extents = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').map(str::trim).collect();
        let [index, logical, physical, length, rest @ ..] = fields.as_slice() else {
            continue;
        };
        if index.parse::<u32>().is_err() {
            continue;
        }
        let flags = rest.last().copied().unwrap_or_default();
        if let Some(flag) = UNSAFE_FLAGS.iter().find(|flag| flags.contains(*flag)) {
            return Err(format!("extent {index} is marked \"{flag}\""));
        }
        let start = |range: &str| {
            range
                .split("..")
                .next()
                .and_then(|n| n.trim().parse::<u64>().ok())
                .ok_or_else(|| format!("unreadable range \"{range}\""))
        };
        let len: u64 = length
            .parse()
            .map_err(|_| format!("unreadable length \"{length}\""))?;
        extents.push(FileExtent {
            file_offset: start(logical)? * UNIT,
            device_offset: start(physical)? * UNIT,
            len: len * UNIT,
        });
    }
    if extents.is_empty() {
        return Err("no extents listed".to_string());
    }
    Ok(extents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAGMENTED: &str = "Filesystem type is: ef53\n\
        File size of journal.bin is 4194304 (8192 blocks of 512 bytes)\n \
        ext:     logical_offset:        physical_offset: length:   expected: flags:\n   \
        0:        0..    3151:  428523464.. 428526615:   3152:            \n   \
        1:     3152..    6231:  428789920.. 428792999:   3080:  428526616:\n   \
        2:     6232..    8191:  428888056.. 428890015:   1960:  428793000: last,eof\n\
        journal.bin: 3 extents found\n";

    #[test]
    fn parses_fragmented_extents_in_bytes() {
        let extents = parse_filefrag(FRAGMENTED).unwrap();
        assert_eq!(extents.len(), 3);
        assert_eq!(
            extents[1],
            FileExtent {
                file_offset: 3152 * 512,
                device_offset: 428_789_920 * 512,
                len: 3080 * 512,
            }
        );
    }

    #[test]
    fn refuses_extents_not_yet_placed_on_disk() {
        let delayed = FRAGMENTED.replace("last,eof", "last,delalloc,eof");
        assert!(parse_filefrag(&delayed).unwrap_err().contains("delalloc"));
    }

    #[test]
    fn refuses_output_without_extents() {
        assert!(parse_filefrag("journal.bin: 0 extents found\n").is_err());
    }
}
