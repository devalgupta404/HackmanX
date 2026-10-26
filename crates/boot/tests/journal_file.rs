//! Locating a real journal file with `filefrag`, which works without root.

#![allow(clippy::unwrap_used)]

use bootshift_boot::journal_file;
use bootshift_core::journal::{self, PlanRecord};
use bootshift_disk::JournalFile;
use bootshift_fs::run::SystemRunner;

#[test]
fn extents_of_a_created_journal_cover_the_whole_file_in_order() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let path = dir.path().join("EFI/bootshift/journal.bin");
    let plan = PlanRecord {
        run_id: 9,
        payload: b"{}".to_vec(),
    };

    journal_file::create(&path, &journal::initial_file_contents(&plan)).unwrap();
    let extents = journal_file::extents(&SystemRunner, &path, 1 << 20).unwrap();

    let mut covered = 0;
    for extent in &extents {
        assert_eq!(extent.file_offset, covered);
        assert!(extent.device_offset >= 1 << 20);
        covered += extent.len;
    }
    assert_eq!(covered, journal::FILE_LEN);
    assert!(JournalFile::new(extents).is_ok());
}
