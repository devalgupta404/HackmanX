//! Whether the desktop asks for reduced motion. Motion here only shows what changed, and all of
//! it is switched off when the user has turned animations off.

use std::process::Command;
use std::sync::OnceLock;

static REDUCED: OnceLock<bool> = OnceLock::new();

pub fn reduced() -> bool {
    *REDUCED.get_or_init(detect)
}

/// `BOOTSHIFT_REDUCED_MOTION=1` forces it on. Otherwise GNOME's animation switch decides, and
/// a desktop that can't be asked keeps the (small) motion on.
fn detect() -> bool {
    if let Ok(value) = std::env::var("BOOTSHIFT_REDUCED_MOTION") {
        return value == "1";
    }
    Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "false")
        .unwrap_or(false)
}
