//! The partition table under the disk. Slivers are unreadable inside the bar, so this is where
//! every partition can be read, compared and picked.

use bootshift_core::layout::{Guid, Layout, Partition, Target};
use eframe::egui::{self, Align2, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::disk_bar::{Command, deletable};
use crate::editing::is_changed;
use crate::format::{short_name, size};
use crate::grid::{self, Column};
use crate::theme::{self, Face, Weight};
use crate::widgets;

const COLUMNS: [Column; 6] = [
    Column::Fixed(11.0),
    Column::Flex {
        min: 105.0,
        fr: 1.7,
    },
    Column::Fixed(105.0),
    Column::Flex { min: 75.0, fr: 1.0 },
    Column::Flex { min: 90.0, fr: 1.0 },
    Column::Fixed(135.0),
];
const GAP: f32 = 15.0;
const PAD_X: f32 = 14.0;
const ROW_HEIGHT: f32 = 34.0;
const HEADER_HEIGHT: f32 = 30.0;

/// The table's result for the frame: the row the user picked and any right-click menu choice.
#[derive(Default)]
pub struct TableOutput {
    pub picked: Option<Guid>,
    pub command: Option<Command>,
}

/// What the change column says: `will be removed`, `locked`, `unchanged`, or the size change.
pub fn change(layout: &Layout, target: &Target, partition: &Partition) -> (String, bool) {
    if !target.contains_key(&partition.unique_guid) {
        return ("will be removed".to_string(), true);
    }
    if !is_changed(layout, target, partition.unique_guid) {
        let word = if partition.content.can_move() {
            "unchanged"
        } else {
            "locked"
        };
        return (word.to_string(), false);
    }
    let after = target
        .get(&partition.unique_guid)
        .copied()
        .unwrap_or(partition.extent);
    let before = partition.extent;
    let mut parts = Vec::new();
    if after.len != before.len {
        let sign = if after.len > before.len { "+" } else { "−" };
        let delta = bootshift_core::units::Sectors(after.len.0.abs_diff(before.len.0));
        parts.push(format!("{sign}{}", size(layout, delta)));
    }
    if after.start != before.start {
        parts.push("moves".to_string());
    }
    (parts.join(", "), true)
}

/// Draws the table. Returns the partition the user picked and any right-click menu choice.
pub fn show(ui: &mut Ui, layout: &Layout, target: &Target, selected: Option<Guid>) -> TableOutput {
    let mut picked = None;
    let mut command = None;
    let width = ui.available_width();
    let height = HEADER_HEIGHT + ROW_HEIGHT * layout.partitions.len() as f32;
    let (frame, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter().with_clip_rect(frame);
    painter.rect_filled(frame, 4, theme::DECK);
    let cols = Columns {
        left: frame.left() + PAD_X,
        spans: grid::columns(width - 2.0 * PAD_X, GAP, &COLUMNS),
    };

    let header = Rect::from_min_size(frame.min, Vec2::new(width, HEADER_HEIGHT));
    painter.rect_filled(header, 0, theme::DECK_HIGH);
    painter.hline(
        header.x_range(),
        header.bottom() - 0.5,
        Stroke::new(1.0, theme::RULE_SOFT),
    );
    for (index, title) in ["", "Partition", "Contents", "Size", "Used", "Change"]
        .iter()
        .enumerate()
    {
        if title.is_empty() {
            continue;
        }
        let galley = widgets::galley(ui, *title, theme::SANS, 10.8, theme::FAINT);
        let (align, at) = if index == 5 {
            (
                Align2::RIGHT_CENTER,
                Pos2::new(cols.right(5), header.center().y),
            )
        } else {
            (
                Align2::LEFT_CENTER,
                Pos2::new(cols.x(index), header.center().y),
            )
        };
        widgets::text_at(&painter, align, at, galley);
    }

    for (row, partition) in layout.partitions.iter().enumerate() {
        let top = header.bottom() + ROW_HEIGHT * row as f32;
        let rect = Rect::from_min_size(Pos2::new(frame.left(), top), Vec2::new(width, ROW_HEIGHT));
        let id = Id::unique(("table-row", partition.unique_guid));
        let response = ui
            .interact(rect, id, Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);
        let is_selected = selected == Some(partition.unique_guid);
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Row,
                true,
                is_selected,
                format!("Partition {}", short_name(partition)),
            )
        });
        if response.clicked() {
            picked = Some(partition.unique_guid);
        }
        let deleted = !target.contains_key(&partition.unique_guid);
        let guid = partition.unique_guid;
        let can_delete = deletable(partition.content);
        let mut chosen = None;
        response.context_menu(|ui| {
            if deleted {
                if ui.button("Keep this partition").clicked() {
                    chosen = Some(Command::Restore(guid));
                }
            } else if ui
                .add_enabled(can_delete, egui::Button::new("Remove this partition"))
                .clicked()
            {
                chosen = Some(Command::Delete(guid));
            }
            ui.add_enabled(false, egui::Button::new("Format as…  (coming soon)"));
        });
        if chosen.is_some() {
            command = chosen;
        }
        let is_selected = selected == Some(partition.unique_guid);
        if is_selected {
            painter.rect_filled(rect, 0, theme::alpha(theme::SIGNAL, 0.06));
            painter.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())),
                0,
                theme::SIGNAL,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, 0, theme::DECK_HIGH);
        }
        if row + 1 < layout.partitions.len() {
            painter.hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                Stroke::new(1.0, theme::RULE_SOFT),
            );
        }
        paint_row(ui, &painter, (layout, target, partition), rect, &cols);
        if widgets::shows_focus(ui, &response) {
            ui.painter().rect_stroke(
                rect.shrink(2.0),
                2,
                Stroke::new(2.0, theme::SIGNAL),
                StrokeKind::Inside,
            );
        }
    }
    ui.painter()
        .rect_stroke(frame, 4, Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
    TableOutput { picked, command }
}

struct Columns {
    left: f32,
    spans: Vec<(f32, f32)>,
}

impl Columns {
    fn x(&self, index: usize) -> f32 {
        self.left + self.spans.get(index).map_or(0.0, |s| s.0)
    }

    fn width(&self, index: usize) -> f32 {
        self.spans.get(index).map_or(0.0, |s| s.1)
    }

    fn right(&self, index: usize) -> f32 {
        self.x(index) + self.width(index)
    }
}

fn paint_row(
    ui: &Ui,
    painter: &eframe::egui::Painter,
    (layout, target, partition): (&Layout, &Target, &Partition),
    rect: Rect,
    cols: &Columns,
) {
    let middle = rect.center().y;
    let swatch = Rect::from_center_size(Pos2::new(cols.x(0) + 5.5, middle), Vec2::splat(10.0));
    painter.rect_filled(swatch, 2, theme::content_colour(partition.content));

    let extent = target
        .get(&partition.unique_guid)
        .copied()
        .unwrap_or(partition.extent);
    let used = if partition.used.0 > 0 {
        size(layout, partition.used)
    } else {
        "empty".to_string()
    };
    let deleted = !target.contains_key(&partition.unique_guid);
    let name_colour = if deleted { theme::DIM } else { theme::INK };
    let cells = [
        (
            1,
            short_name(partition),
            Face::Sans(Weight::Medium),
            12.3,
            name_colour,
        ),
        (
            2,
            partition.content.name().to_string(),
            theme::MONO,
            11.4,
            theme::DIM,
        ),
        (3, size(layout, extent.len), theme::MONO, 11.4, theme::DIM),
        (4, used, theme::MONO, 11.4, theme::DIM),
    ];
    for (column, text, face, px, colour) in cells {
        let galley = widgets::elided(ui, text, face, px, colour, cols.width(column));
        widgets::text_at(
            painter,
            Align2::LEFT_CENTER,
            Pos2::new(cols.x(column), middle),
            galley,
        );
    }
    let (text, changed) = change(layout, target, partition);
    let colour = if deleted {
        theme::STOP
    } else if changed {
        theme::SIGNAL
    } else {
        theme::FAINT
    };
    let galley = widgets::elided(ui, text, theme::MONO, 11.1, colour, cols.width(5));
    widgets::text_at(
        painter,
        Align2::RIGHT_CENTER,
        Pos2::new(cols.right(5), middle),
        galley,
    );
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::Extent;
    use bootshift_core::units::{Lba, Sectors};

    use super::*;
    use crate::demo::demo_layout;

    fn unchanged(layout: &Layout) -> Target {
        layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect()
    }

    #[test]
    fn untouched_partitions_say_unchanged_or_locked() {
        let layout = demo_layout();
        let target = unchanged(&layout);
        assert_eq!(change(&layout, &target, &layout.partitions[0]).0, "locked");
        assert_eq!(
            change(&layout, &target, &layout.partitions[1]).0,
            "unchanged"
        );
    }

    #[test]
    fn a_shrink_and_a_move_are_both_listed() {
        let layout = demo_layout();
        let mut target = unchanged(&layout);
        let linux = &layout.partitions[4];
        let gib = 2048 * 1024;
        target.insert(
            linux.unique_guid,
            Extent::new(
                Lba(linux.extent.start.0 - gib),
                Sectors(linux.extent.len.0 - 12 * gib),
            ),
        );
        assert_eq!(
            change(&layout, &target, linux),
            ("−12.0 GiB, moves".to_string(), true)
        );
    }
}
