//! A pretend resize tool writes through a guarded window. Wherever the power is cut, rolling
//! back must restore every byte that held data when the run started.

#![allow(clippy::unwrap_used)]

use std::ops::Range;

use bootshift_disk::{
    BlockDevice, CrashDisk, FileExtent, GuardedDevice, JournalFile, MemDisk, PendingWrites,
    roll_back,
};

const KIB: u64 = 1024;
const WINDOW: Range<u64> = 64 * KIB..192 * KIB;
const AREA_AT: u64 = 192 * KIB;
const AREA_LEN: u64 = 64 * KIB;
const GUARD_ID: u64 = 0xC0DE;

fn area() -> JournalFile {
    JournalFile::covering(
        vec![FileExtent {
            file_offset: 0,
            device_offset: AREA_AT,
            len: AREA_LEN,
        }],
        AREA_LEN,
    )
    .unwrap()
}

/// Window offsets that held data before the tool ran.
fn protected() -> Vec<Range<u64>> {
    vec![0..16 * KIB, 40 * KIB..72 * KIB, 120 * KIB..128 * KIB]
}

fn original_disk() -> MemDisk {
    let bytes = (0..256 * KIB)
        .map(|i| (i.wrapping_mul(97) >> 4) as u8)
        .collect();
    MemDisk::from_bytes(bytes, 512).unwrap()
}

/// What a resize tool might do: scattered writes of various sizes, some overlapping earlier
/// ones, with a couple of flushes.
fn run_tool<D: BlockDevice>(device: &mut D) {
    let writes: [(u64, u64, u8); 8] = [
        (0, 4 * KIB, 0xA1),
        (60 * KIB, 8 * KIB, 0xA2),
        (2 * KIB, KIB, 0xA3),
        (100 * KIB, 4 * KIB, 0xA4),
        (44 * KIB, 20 * KIB, 0xA5),
        (124 * KIB, 4 * KIB, 0xA6),
        (12 * KIB, 8 * KIB, 0xA7),
        (0, 2 * KIB, 0xA8),
    ];
    for (index, (offset, len, byte)) in writes.into_iter().enumerate() {
        device.write_at(offset, &vec![byte; len as usize]).unwrap();
        if index % 3 == 2 {
            device.flush().unwrap();
        }
    }
    device.flush().unwrap();
}

fn protected_bytes_match(original: &MemDisk, restored: &MemDisk) -> bool {
    protected().into_iter().all(|range| {
        let on_disk = (WINDOW.start + range.start) as usize..(WINDOW.start + range.end) as usize;
        original.bytes()[on_disk.clone()] == restored.bytes()[on_disk]
    })
}

fn guarded<D: BlockDevice>(disk: &mut D) -> GuardedDevice<'_, D> {
    GuardedDevice::new(disk, WINDOW, protected(), area(), AREA_LEN, GUARD_ID).unwrap()
}

#[test]
fn writes_pass_through_to_the_window() {
    let mut disk = original_disk();
    run_tool(&mut guarded(&mut disk));

    let start = (WINDOW.start + 44 * KIB) as usize;
    assert_eq!(
        &disk.bytes()[start..start + 20 * KIB as usize],
        &[0xA5; 20 * 1024][..]
    );
}

#[test]
fn rolling_back_a_finished_run_restores_the_original_data() {
    let original = original_disk();
    let mut disk = original.clone();
    run_tool(&mut guarded(&mut disk));

    roll_back(&mut disk, WINDOW.start, &area(), AREA_LEN, GUARD_ID).unwrap();

    assert!(protected_bytes_match(&original, &disk));
}

#[test]
fn rolling_back_after_any_crash_restores_the_original_data() {
    let original = original_disk();
    let mut recorded = CrashDisk::new(original.clone());
    run_tool(&mut guarded(&mut recorded));

    let outcomes = [PendingWrites::Lost, PendingWrites::AllLanded]
        .into_iter()
        .chain((0..24).map(|seed| PendingWrites::Scrambled { seed }));
    for outcome in outcomes {
        for point in recorded.crash_points() {
            let mut image = recorded.crash_image(point, outcome);
            roll_back(&mut image, WINDOW.start, &area(), AREA_LEN, GUARD_ID).unwrap();
            assert!(
                protected_bytes_match(&original, &image),
                "point {point}, {outcome:?}: data was not restored"
            );
        }
    }
}

#[test]
fn rolling_back_twice_is_the_same_as_once() {
    let original = original_disk();
    let mut disk = original.clone();
    run_tool(&mut guarded(&mut disk));

    roll_back(&mut disk, WINDOW.start, &area(), AREA_LEN, GUARD_ID).unwrap();
    roll_back(&mut disk, WINDOW.start, &area(), AREA_LEN, GUARD_ID).unwrap();

    assert!(protected_bytes_match(&original, &disk));
}

#[test]
fn an_undo_area_inside_the_window_is_refused() {
    let mut disk = original_disk();
    let inside = JournalFile::covering(
        vec![FileExtent {
            file_offset: 0,
            device_offset: WINDOW.start,
            len: AREA_LEN,
        }],
        AREA_LEN,
    )
    .unwrap();
    assert!(
        GuardedDevice::new(&mut disk, WINDOW, protected(), inside, AREA_LEN, GUARD_ID).is_err()
    );
}
