//! The gutter between the "Now" bar and the "After" bar. Every partition whose start changes
//! gets a dimension line spanning exactly the distance it travels, arrowhead at the
//! destination, the way a drawing marks a measurement. One line per moved partition, each on
//! its own row, so none overlap.

use bootshift_core::layout::{Layout, Partition, Target};
use bootshift_core::units::{Lba, Sectors};
use eframe::egui::{Align2, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

use crate::format::{mover_name, size};
use crate::theme;
use crate::widgets;

const LANE: f32 = 22.5;
/// Room above the first line for its label to sit clear of the bar overhead.
const FIRST_LANE: f32 = 22.0;
const EMPTY_HEIGHT: f32 = 13.5;

pub struct Move<'a> {
    pub partition: &'a Partition,
    pub from: Lba,
    pub to: Lba,
}

impl Move<'_> {
    pub fn distance(&self) -> Sectors {
        self.from.distance(self.to)
    }

    pub fn label(&self, layout: &Layout) -> String {
        format!(
            "{} moves {}",
            mover_name(self.partition),
            size(layout, self.distance())
        )
    }
}

/// The partitions whose start sector changed, in disk order.
pub fn moves<'a>(layout: &'a Layout, target: &Target) -> Vec<Move<'a>> {
    layout
        .partitions
        .iter()
        .filter_map(|partition| {
            let to = target.get(&partition.unique_guid)?.start;
            (to != partition.extent.start).then_some(Move {
                partition,
                from: partition.extent.start,
                to,
            })
        })
        .collect()
}

pub fn height(moves: usize) -> f32 {
    if moves == 0 {
        EMPTY_HEIGHT
    } else {
        moves as f32 * LANE + 16.5
    }
}

pub fn show(ui: &mut Ui, layout: &Layout, target: &Target) {
    let moves = moves(layout, target);
    let width = ui.available_width();
    if moves.is_empty() {
        ui.add_space(2.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, EMPTY_HEIGHT), Sense::hover());
        dashed(ui, rect);
        ui.add_space(7.5);
        return;
    }
    ui.add_space(5.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height(moves.len())), Sense::hover());
    // The bars inside their 1px borders are what the lines measure against.
    let span = rect.shrink2(Vec2::new(1.0, 0.0));
    let usable = layout.geometry.usable;
    let x_of = |lba: Lba| {
        let share = lba.0.saturating_sub(usable.start.0) as f64 / usable.len.0.max(1) as f64;
        span.left() + span.width() * share as f32
    };
    for (lane, step) in moves.iter().enumerate() {
        let y = (rect.top() + FIRST_LANE + lane as f32 * LANE).round() + 0.5;
        let (from, to) = (x_of(step.from), x_of(step.to));
        let (left, right) = (
            from.min(to),
            from.max(to).max(from.min(to) + span.width() * 0.003),
        );
        dimension_line(ui, left, right, y, to > from, &step.label(layout));
    }
    ui.add_space(13.5);
}

fn dimension_line(ui: &Ui, left: f32, right: f32, y: f32, rightward: bool, label: &str) {
    let painter = ui.painter();
    let line = Stroke::new(1.0, theme::SIGNAL_DIM);
    painter.hline(left..=right, y, line);
    for x in [left + 0.5, right - 0.5] {
        painter.vline(x, (y - 3.5)..=(y + 3.5), line);
    }
    let (tip, back) = if rightward {
        (right, right - 7.0)
    } else {
        (left, left + 7.0)
    };
    painter.add(Shape::convex_polygon(
        vec![
            Pos2::new(tip, y),
            Pos2::new(back, y - 4.5),
            Pos2::new(back, y + 4.5),
        ],
        theme::SIGNAL,
        Stroke::NONE,
    ));
    let text = widgets::galley(ui, label, theme::MONO, 10.0, theme::SIGNAL);
    let centre = Pos2::new((left + right) / 2.0, y - 3.75);
    let backing = Align2::CENTER_BOTTOM
        .anchor_size(centre, text.size())
        .expand2(Vec2::new(4.5, 0.0));
    painter.rect_filled(backing, 0, theme::DECK);
    widgets::text_at(painter, Align2::CENTER_BOTTOM, centre, text);
}

fn dashed(ui: &Ui, rect: Rect) {
    let y = rect.bottom() - 0.5;
    let stroke = Stroke::new(1.0, theme::RULE_SOFT);
    let mut x = rect.left();
    while x < rect.right() {
        ui.painter()
            .hline(x..=(x + 3.0).min(rect.right()), y, stroke);
        x += 6.0;
    }
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::Extent;

    use super::*;
    use crate::demo::demo_layout;

    #[test]
    fn nothing_moved_draws_nothing() {
        let layout = demo_layout();
        let target: Target = layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect();
        assert!(moves(&layout, &target).is_empty());
        assert_eq!(height(0), 13.5);
    }

    #[test]
    fn the_label_states_exactly_the_distance_travelled() {
        let layout = demo_layout();
        let mut target: Target = layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect();
        let recovery = &layout.partitions[3];
        let gib = 2048 * 1024;
        let to = Lba(recovery.extent.start.0 - 83 * gib - gib / 2);
        target.insert(recovery.unique_guid, Extent::new(to, recovery.extent.len));
        let found = moves(&layout, &target);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label(&layout), "Windows recovery moves 83.5 GiB");
    }

    #[test]
    fn an_unnamed_partition_is_called_by_its_number() {
        let layout = demo_layout();
        let mut target: Target = layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect();
        let swap = &layout.partitions[5];
        target.insert(
            swap.unique_guid,
            Extent::new(Lba(swap.extent.start.0 + 2048), swap.extent.len),
        );
        assert_eq!(
            moves(&layout, &target)[0].label(&layout),
            "partition 6 moves 1.0 MiB"
        );
    }

    #[test]
    fn the_gutter_grows_one_row_per_move() {
        assert_eq!(height(2), 2.0 * 22.5 + 16.5);
    }
}
