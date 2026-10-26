//! Writing the "newc" cpio archives the Linux kernel unpacks as an initramfs.

const MAGIC: &str = "070701";
const TRAILER: &str = "TRAILER!!!";

const FILE_TYPE: u32 = 0o170_000;
const DIRECTORY: u32 = 0o040_000;
const REGULAR: u32 = 0o100_000;
const SYMLINK: u32 = 0o120_000;
const CHAR_DEVICE: u32 = 0o020_000;

/// Builds an archive in memory. Paths are written without a leading `/`.
#[derive(Default)]
pub struct CpioWriter {
    bytes: Vec<u8>,
    next_inode: u32,
}

impl CpioWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn directory(&mut self, path: &str, permissions: u32) {
        self.entry(path, DIRECTORY | permissions, 0, 0, &[]);
    }

    pub fn file(&mut self, path: &str, permissions: u32, contents: &[u8]) {
        self.entry(path, REGULAR | permissions, 0, 0, contents);
    }

    pub fn symlink(&mut self, path: &str, target: &str) {
        self.entry(path, SYMLINK | 0o777, 0, 0, target.as_bytes());
    }

    pub fn char_device(&mut self, path: &str, permissions: u32, major: u32, minor: u32) {
        self.entry(path, CHAR_DEVICE | permissions, major, minor, &[]);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.next_inode = 0;
        self.entry(TRAILER, 0, 0, 0, &[]);
        self.bytes
    }

    fn entry(&mut self, path: &str, mode: u32, rdev_major: u32, rdev_minor: u32, data: &[u8]) {
        let name = path.trim_start_matches('/');
        let nlink = if mode & FILE_TYPE == DIRECTORY { 2 } else { 1 };
        self.next_inode += 1;
        let fields = [
            self.next_inode,
            mode,
            0,
            0,
            nlink,
            0,
            data.len() as u32,
            0,
            0,
            rdev_major,
            rdev_minor,
            name.len() as u32 + 1,
            0,
        ];
        self.bytes.extend(MAGIC.as_bytes());
        for field in fields {
            self.bytes.extend(format!("{field:08X}").as_bytes());
        }
        self.bytes.extend(name.as_bytes());
        self.bytes.push(0);
        self.pad();
        self.bytes.extend(data);
        self.pad();
    }

    fn pad(&mut self) {
        let padded = self.bytes.len().next_multiple_of(4);
        self.bytes.resize(padded, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields_are_eight_hex_digits_after_the_magic() {
        let mut writer = CpioWriter::new();
        writer.file("/init", 0o755, b"hello");
        let archive = writer.finish();

        assert!(archive.starts_with(b"070701"));
        let header = std::str::from_utf8(&archive[6..110]).unwrap();
        assert_eq!(&header[8..16], "000081ED");
        assert_eq!(&header[48..56], "00000005");
        assert_eq!(archive.len() % 4, 0);
    }

    #[test]
    fn ends_with_the_trailer() {
        let archive = CpioWriter::new().finish();
        assert!(
            archive
                .windows(TRAILER.len())
                .any(|w| w == TRAILER.as_bytes())
        );
    }
}
