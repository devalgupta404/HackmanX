//! Helpers shared by `send` and `receive`: opening a disk or image, and waiting for one message.

use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bootshift_disk::{Access, RealDevice};
use bootshift_migrate_core::{Epoch, FrameSeq, Message, decode};
use bootshift_migrate_net::{Link, LinkError, TcpLink};

/// Opens `path` as a block device (block size from the kernel) or, if it is a regular file, as a
/// disk image with `block_size`-byte blocks. The source is only ever opened read-only (M1).
pub fn open_device(path: &Path, access: Access, block_size: u32) -> Result<RealDevice> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("looking at {}", path.display()))?;
    if meta.file_type().is_block_device() {
        RealDevice::open(path, access)
            .with_context(|| format!("opening block device {}", path.display()))
    } else {
        RealDevice::open_image(path, access, block_size)
            .with_context(|| format!("opening image {}", path.display()))
    }
}

/// Blocks until one message arrives, timing out or failing if the link cuts first.
pub fn recv_message(
    link: &mut TcpLink,
    timeout: Duration,
) -> Result<(Epoch, FrameSeq, Message)> {
    let deadline = Instant::now() + timeout;
    loop {
        match link.recv() {
            Ok(Some(frame)) => return decode(&frame).context("decoding a frame from the peer"),
            Ok(None) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the peer");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(LinkError::Cut) => bail!("the link cut while waiting for the peer"),
        }
    }
}
