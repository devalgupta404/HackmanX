//! Sending the machine into the recovery system through the distro's own signed GRUB.
//!
//! Signed GRUB reads its config only from `EFI/<vendor>/grub.cfg` on the ESP, so Kitten
//! replaces that file with one that boots the recovery kernel, keeping the original beside it.
//! Every way of reaching the distro's shim (its boot entry, a reset boot order, the fallback
//! loader) then lands in recovery until the original is put back.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const CONFIG: &str = "grub.cfg";
const BACKUP_SUFFIX: &str = ".bootshift-orig";
const TEMP_SUFFIX: &str = ".bootshift-tmp";

/// A GRUB config that boots the recovery kernel and initramfs from `EFI/bootshift/` straight
/// away, with no menu.
pub fn recovery_config(esp_fs_uuid: &str, kernel_args: &str) -> String {
    format!(
        "set timeout=0\n\
         search --no-floppy --fs-uuid --set=root {esp_fs_uuid}\n\
         linux /EFI/bootshift/vmlinuz {kernel_args}\n\
         initrd /EFI/bootshift/initrd.img\n\
         boot\n"
    )
}

/// The vendor directories under `EFI/` that hold a GRUB config, such as `ubuntu` or `fedora`.
pub fn vendors_with_config(esp: &Path) -> io::Result<Vec<String>> {
    let mut vendors = Vec::new();
    for entry in fs::read_dir(esp.join("EFI"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let has_config = entry.path().join(CONFIG).is_file();
        if has_config && name != "bootshift" {
            vendors.push(name);
        }
    }
    vendors.sort();
    Ok(vendors)
}

/// The vendor directory whose GRUB starts this system. With several, the one named like the
/// firmware's current boot entry (such as `ubuntu`); `None` if that can't be told.
pub fn pick_vendor(vendors: &[String], current_entry_label: Option<&str>) -> Option<String> {
    match vendors {
        [only] => Some(only.clone()),
        _ => {
            let label = current_entry_label?.to_lowercase();
            vendors.iter().find(|v| v.to_lowercase() == label).cloned()
        }
    }
}

/// Replaces `EFI/<vendor>/grub.cfg` with `config`, first saving the original beside it. Safe to
/// repeat: once a backup exists it is never overwritten, so the true original is kept.
pub fn install_redirect(esp: &Path, vendor: &str, config: &str) -> io::Result<()> {
    let (current, backup) = paths(esp, vendor);
    if !backup.exists() {
        let original = fs::read(&current)?;
        write_atomically(&backup, &original)?;
    }
    write_atomically(&current, config.as_bytes())
}

/// Puts the original config back. Safe to repeat, and does nothing if no redirect was made.
pub fn remove_redirect(esp: &Path, vendor: &str) -> io::Result<()> {
    let (current, backup) = paths(esp, vendor);
    if backup.exists() {
        fs::rename(&backup, &current)?;
        sync_parent(&current)?;
    }
    Ok(())
}

fn paths(esp: &Path, vendor: &str) -> (PathBuf, PathBuf) {
    let current = esp.join("EFI").join(vendor).join(CONFIG);
    let backup = esp
        .join("EFI")
        .join(vendor)
        .join(format!("{CONFIG}{BACKUP_SUFFIX}"));
    (current, backup)
}

/// Writes a temporary file, flushes it, renames it over `path` and flushes the directory, so
/// `path` holds either its old or its new contents, never a mix.
fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut temp_name = path.as_os_str().to_owned();
    temp_name.push(TEMP_SUFFIX);
    let temp = PathBuf::from(temp_name);
    let mut file = File::create(&temp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => File::open(parent)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str =
        "search.fs_uuid 1234 root\nset prefix=($root)'/boot/grub'\nconfigfile $prefix/grub.cfg\n";

    fn esp() -> tempfile::TempDir {
        let esp = tempfile::tempdir().unwrap();
        fs::create_dir_all(esp.path().join("EFI/ubuntu")).unwrap();
        fs::create_dir_all(esp.path().join("EFI/Microsoft/Boot")).unwrap();
        fs::write(esp.path().join("EFI/ubuntu/grub.cfg"), ORIGINAL).unwrap();
        esp
    }

    fn config(esp: &Path) -> String {
        fs::read_to_string(esp.join("EFI/ubuntu/grub.cfg")).unwrap()
    }

    #[test]
    fn recovery_config_boots_the_recovery_kernel_without_a_menu() {
        let text = recovery_config("8A3C-1F2E", "bootshift.disk=x");
        assert!(text.starts_with("set timeout=0\n"));
        assert!(text.contains("--fs-uuid --set=root 8A3C-1F2E"));
        assert!(text.contains("linux /EFI/bootshift/vmlinuz bootshift.disk=x"));
    }

    #[test]
    fn finds_vendors_that_have_a_grub_config() {
        let esp = esp();
        assert_eq!(vendors_with_config(esp.path()).unwrap(), ["ubuntu"]);
    }

    #[test]
    fn picks_the_only_vendor_or_the_one_named_like_the_current_entry() {
        let one = ["ubuntu".to_string()];
        let two = ["fedora".to_string(), "ubuntu".to_string()];
        assert_eq!(pick_vendor(&one, None).as_deref(), Some("ubuntu"));
        assert_eq!(pick_vendor(&two, Some("Ubuntu")).as_deref(), Some("ubuntu"));
        assert_eq!(pick_vendor(&two, None), None);
        assert_eq!(pick_vendor(&two, Some("Windows Boot Manager")), None);
    }

    #[test]
    fn redirect_then_remove_restores_the_original_exactly() {
        let esp = esp();

        install_redirect(esp.path(), "ubuntu", "recovery\n").unwrap();
        assert_eq!(config(esp.path()), "recovery\n");

        remove_redirect(esp.path(), "ubuntu").unwrap();
        assert_eq!(config(esp.path()), ORIGINAL);
        assert!(
            !esp.path()
                .join("EFI/ubuntu/grub.cfg.bootshift-orig")
                .exists()
        );
    }

    #[test]
    fn repeating_the_redirect_keeps_the_true_original() {
        let esp = esp();
        install_redirect(esp.path(), "ubuntu", "recovery\n").unwrap();
        install_redirect(esp.path(), "ubuntu", "recovery again\n").unwrap();

        remove_redirect(esp.path(), "ubuntu").unwrap();

        assert_eq!(config(esp.path()), ORIGINAL);
    }

    #[test]
    fn removing_twice_or_without_a_redirect_changes_nothing() {
        let esp = esp();
        remove_redirect(esp.path(), "ubuntu").unwrap();
        install_redirect(esp.path(), "ubuntu", "recovery\n").unwrap();
        remove_redirect(esp.path(), "ubuntu").unwrap();
        remove_redirect(esp.path(), "ubuntu").unwrap();

        assert_eq!(config(esp.path()), ORIGINAL);
    }
}
