//! Adapting a migrated guest to its new hardware (implementation I6, the `Adapt` step): rewriting
//! the device-name references in config files that moved controller, done on parsed files rather
//! than with `sed`. Identity that the byte copy preserves (filesystem UUIDs, PARTUUIDs) needs no
//! change (M9), so a `UUID=` or `PARTUUID=` entry is left exactly as it was; only a bare
//! `/dev/<name>` reference, which is tied to the old controller, is remapped.
//!
//! Pure and total: it never does I/O, so the whole rewrite is unit-tested, and the list of changes
//! it returns is what the receipt records and the undo log can reverse.

/// One device-name change the adapt made, kept for the receipt and for undo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRewrite {
    pub from: String,
    pub to: String,
}

/// The result of rewriting a file: its new text and the changes made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapted {
    pub text: String,
    pub changes: Vec<DeviceRewrite>,
}

/// Rewrites the device column of an `/etc/fstab`, mapping each old `/dev/<name>` spec to its new
/// name. `UUID=`, `PARTUUID=`, `LABEL=` specs, comments and blank lines are left untouched. Each
/// `renames` pair is (old device path, new device path), e.g. ("/dev/sda2", "/dev/vda2").
pub fn rewrite_fstab(contents: &str, renames: &[(String, String)]) -> Adapted {
    rewrite_first_field(contents, renames, is_device_spec)
}

/// Rewrites bare `/dev/<name>` references anywhere in a GRUB `grub.cfg` (the `set root=` hints and
/// `root=/dev/...` kernel arguments some setups still carry), mapping old to new. UUID-based
/// `search --fs-uuid` lines are untouched because they carry no device name.
pub fn rewrite_grub(contents: &str, renames: &[(String, String)]) -> Adapted {
    let mut changes = Vec::new();
    let mut out = String::with_capacity(contents.len());
    for (i, line) in contents.split_inclusive('\n').enumerate() {
        let _ = i;
        let mut rewritten = line.to_string();
        for (from, to) in renames {
            if contains_token(&rewritten, from) {
                rewritten = replace_token(&rewritten, from, to);
                changes.push(DeviceRewrite {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
        out.push_str(&rewritten);
    }
    Adapted { text: out, changes }
}

fn rewrite_first_field(
    contents: &str,
    renames: &[(String, String)],
    is_spec: fn(&str) -> bool,
) -> Adapted {
    let mut changes = Vec::new();
    let mut out = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        let (body, newline) = split_newline(line);
        let trimmed = body.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            out.push_str(line);
            continue;
        }
        let indent = &body[..body.len() - trimmed.len()];
        let mut fields: Vec<&str> = trimmed.split_whitespace().collect();
        if let Some(first) = fields.first().copied() {
            if is_spec(first) {
                if let Some((_, to)) = renames.iter().find(|(from, _)| from == first) {
                    fields[0] = to;
                    changes.push(DeviceRewrite {
                        from: first.to_string(),
                        to: to.clone(),
                    });
                }
            }
        }
        out.push_str(indent);
        out.push_str(&fields.join(" "));
        out.push_str(newline);
    }
    Adapted { text: out, changes }
}

fn is_device_spec(field: &str) -> bool {
    field.starts_with("/dev/")
}

fn split_newline(line: &str) -> (&str, &str) {
    if let Some(stripped) = line.strip_suffix('\n') {
        (stripped, "\n")
    } else {
        (line, "")
    }
}

/// Whether `needle` appears in `text` as a whole token (bounded by start/end or non-path chars), so
/// `/dev/sda` does not match inside `/dev/sda2`.
fn contains_token(text: &str, needle: &str) -> bool {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(pos) = text[start..].find(needle) {
        let at = start + pos;
        let before_ok = at == 0 || !is_token_char(bytes[at - 1]);
        let after = at + needle.len();
        let after_ok = after >= bytes.len() || !is_token_char(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = at + needle.len();
    }
    false
}

fn replace_token(text: &str, needle: &str, replacement: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut cursor = 0;
    while let Some(pos) = text[start..].find(needle) {
        let at = start + pos;
        let before_ok = at == 0 || !is_token_char(bytes[at - 1]);
        let after = at + needle.len();
        let after_ok = after >= bytes.len() || !is_token_char(bytes[after]);
        if before_ok && after_ok {
            out.push_str(&text[cursor..at]);
            out.push_str(replacement);
            cursor = after;
        }
        start = at + needle.len();
    }
    out.push_str(&text[cursor..]);
    out
}

/// Characters that are part of a device path token, so `/dev/sda` and `/dev/sda2` stay distinct.
fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'/' || byte == b'_' || byte == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renames() -> Vec<(String, String)> {
        vec![
            ("/dev/sda1".to_string(), "/dev/vda1".to_string()),
            ("/dev/sda2".to_string(), "/dev/vda2".to_string()),
        ]
    }

    #[test]
    fn uuid_and_comment_lines_are_untouched() {
        let fstab = "# /etc/fstab\nUUID=abc-123 / ext4 defaults 0 1\n\n";
        let adapted = rewrite_fstab(fstab, &renames());
        assert_eq!(adapted.text, fstab);
        assert!(adapted.changes.is_empty());
    }

    #[test]
    fn a_device_spec_is_remapped_and_recorded() {
        let fstab = "/dev/sda2 / ext4 defaults 0 1\n/dev/sda1 /boot/efi vfat umask=0077 0 1\n";
        let adapted = rewrite_fstab(fstab, &renames());
        assert_eq!(
            adapted.text,
            "/dev/vda2 / ext4 defaults 0 1\n/dev/vda1 /boot/efi vfat umask=0077 0 1\n"
        );
        assert_eq!(adapted.changes.len(), 2);
        assert_eq!(adapted.changes[0], DeviceRewrite {
            from: "/dev/sda2".to_string(),
            to: "/dev/vda2".to_string(),
        });
    }

    #[test]
    fn an_unrelated_device_is_left_alone() {
        let fstab = "/dev/sdb1 /data ext4 defaults 0 2\n";
        let adapted = rewrite_fstab(fstab, &renames());
        assert_eq!(adapted.text, fstab);
        assert!(adapted.changes.is_empty());
    }

    #[test]
    fn grub_device_hints_are_remapped_as_whole_tokens() {
        // /dev/sda must not match inside /dev/sda2.
        let grub = "linux /vmlinuz root=/dev/sda2 ro\nset expected=/dev/sda2x\n";
        let adapted = rewrite_grub(grub, &renames());
        assert_eq!(
            adapted.text,
            "linux /vmlinuz root=/dev/vda2 ro\nset expected=/dev/sda2x\n"
        );
        assert_eq!(adapted.changes.len(), 1);
    }

    #[test]
    fn rewriting_preserves_a_missing_final_newline() {
        let fstab = "/dev/sda2 / ext4 defaults 0 1";
        let adapted = rewrite_fstab(fstab, &renames());
        assert_eq!(adapted.text, "/dev/vda2 / ext4 defaults 0 1");
    }
}
