//! Saving a readable copy of the partition table to a file the user picks. The raw GPT backup
//! is the helper's job and goes on the EFI partition; this copy is for the user to keep.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use bootshift_core::layout::{Geometry, Partition};
use eframe::egui::{self, Id, Margin, Modal, Stroke, TextEdit};
use serde::Serialize;

use crate::protocol::Probed;
use crate::theme::{self, Face, Weight};
use crate::widgets::{self, Kind};

#[derive(Serialize)]
struct BackupFile<'a> {
    note: &'a str,
    disk: &'a str,
    size: u64,
    geometry: &'a Geometry,
    partitions: &'a [Partition],
}

pub struct SaveDialog {
    probed: Probed,
    path: String,
    error: Option<String>,
    saved: Option<PathBuf>,
}

impl SaveDialog {
    pub fn new(probed: &Probed) -> Self {
        let name = Path::new(&probed.disk)
            .file_name()
            .map_or_else(|| "disk".into(), |n| n.to_string_lossy().into_owned());
        let folder = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let path = folder.join(format!("bootshift-partition-table-{name}.json"));
        Self {
            probed: probed.clone(),
            path: path.display().to_string(),
            error: None,
            saved: None,
        }
    }

    /// Draws the dialog. Returns false once it should close.
    pub fn show(&mut self, ctx: &egui::Context, is_demo: bool) -> bool {
        let mut open = true;
        let frame = egui::Frame::new()
            .fill(theme::DECK)
            .stroke(Stroke::new(1.0, theme::RULE))
            .corner_radius(4)
            .inner_margin(Margin::same(20));
        let modal = Modal::new(Id::unique("save-table")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0);
            widgets::label(ui, "Save the partition table", Face::Sans(Weight::SemiBold), 15.0, theme::INK);
            ui.add_space(8.0);
            widgets::paragraph(
                ui,
                "A readable copy of the layout as it is now. Keep it somewhere that is not this disk.",
                12.0,
                theme::DIM,
                440.0,
                1.5,
            );
            ui.add_space(14.0);
            widgets::label(ui, "File", theme::SANS, 11.0, theme::FAINT);
            ui.add_space(4.0);
            let field = ui.add(
                TextEdit::singleline(&mut self.path)
                    .font(theme::MONO.at(11.5))
                    .desired_width(440.0)
                    .margin(Margin::symmetric(8, 6)),
            );
            if let Some(error) = &self.error {
                ui.add_space(8.0);
                widgets::paragraph(ui, error, 12.0, theme::STOP, 440.0, 1.45);
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if widgets::button(ui, "Cancel", Kind::Secondary, true).clicked() {
                    open = false;
                }
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if widgets::button(ui, "Save", Kind::Primary, !self.path.trim().is_empty()).clicked()
                    || enter
                {
                    match self.write(is_demo) {
                        Ok(path) => {
                            self.saved = Some(path);
                            open = false;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        open
    }

    pub fn saved_message(&self) -> Option<String> {
        self.saved.as_ref().map(|path| {
            format!(
                "Partition table saved to {}. Keep it somewhere that is not this disk.",
                path.display()
            )
        })
    }

    /// Writes the file, refusing to replace one that is already there.
    fn write(&self, is_demo: bool) -> Result<PathBuf, String> {
        let path = PathBuf::from(self.path.trim());
        let text = backup_text(&self.probed, is_demo).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    "A file with that name is already there. Pick another name.".to_string()
                }
                _ => format!("Kitten couldn't write {}: {error}.", path.display()),
            })?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("Kitten couldn't write {}: {error}.", path.display()))?;
        Ok(path)
    }
}

pub fn backup_text(probed: &Probed, is_demo: bool) -> serde_json::Result<String> {
    let note = if is_demo {
        "Demo data. The real backup is the raw GPT, written to the EFI partition before anything moves."
    } else {
        "A readable copy of the partition table as it is now. The raw GPT also goes on the EFI partition."
    };
    serde_json::to_string_pretty(&BackupFile {
        note,
        disk: &probed.disk,
        size: probed.size,
        geometry: &probed.layout.geometry,
        partitions: &probed.layout.partitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::demo_layout;

    fn probed() -> Probed {
        let layout = demo_layout();
        Probed {
            disk: "/dev/demo".to_string(),
            size: 512 << 30,
            target: layout
                .partitions
                .iter()
                .map(|p| (p.unique_guid, p.extent))
                .collect(),
            layout,
        }
    }

    #[test]
    fn a_demo_backup_says_its_data_is_made_up() {
        let text = backup_text(&probed(), true).unwrap();
        assert!(text.contains("Demo data."));
        assert!(text.contains("\"Windows recovery\""));
    }

    #[test]
    fn saving_never_replaces_an_existing_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("table.json");
        std::fs::write(&path, "keep me").unwrap();
        let mut dialog = SaveDialog::new(&probed());
        dialog.path = path.display().to_string();
        assert!(dialog.write(true).unwrap_err().contains("already there"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[test]
    fn a_saved_backup_can_be_read_back() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("table.json");
        let mut dialog = SaveDialog::new(&probed());
        dialog.path = path.display().to_string();
        assert_eq!(dialog.write(false).unwrap(), path);
        let text = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["partitions"].as_array().map(Vec::len), Some(6));
    }
}
