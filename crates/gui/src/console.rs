//! The recovery screen as the machine shows it during the restart, drawn so people know what
//! to expect. It is deliberately off the app's palette: it mirrors a real Linux text console.

use bootshift_core::units::format_bytes;
use eframe::egui::{
    Color32, CornerRadius, Margin, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, text::LayoutJob,
};

use crate::recovery_run::{Phase, RecoveryRun};
use crate::theme::{self, Face};
use crate::widgets;

const GROUND: Color32 = Color32::from_rgb(0x04, 0x05, 0x05);
const BODY: Color32 = Color32::from_rgb(0xa9, 0xb6, 0xad);
const DIM: Color32 = Color32::from_rgb(0x56, 0x61, 0x57);
const BRIGHT: Color32 = Color32::from_rgb(0xee, 0xf3, 0xee);
const GREEN: Color32 = Color32::from_rgb(0x3f, 0xe0, 0x81);
const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa3, 0x3e);
const RULE: Color32 = Color32::from_rgb(0x14, 0x1a, 0x15);
const BAR_CELLS: usize = 36;
const MIN_HEIGHT: f32 = 225.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Body,
    Dim,
    Bright,
    Green,
    Amber,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    Text(Tone, String),
    Progress { filled: usize, percent: u64 },
    Rule,
}

/// What the console shows for the run as it stands.
fn lines(run: &RecoveryRun) -> Vec<Line> {
    if let Phase::PowerCut { .. } = run.phase {
        return run
            .power_up_lines()
            .into_iter()
            .map(|(green, text)| Line::Text(if green { Tone::Green } else { Tone::Dim }, text))
            .collect();
    }
    let share = run.share();
    let done = run.phase == Phase::Finished;
    let moved = (run.total_bytes as f64 * share).round() as u64;
    let mut lines = vec![
        Line::Text(Tone::Bright, "Kitten recovery".to_string()),
        Line::Text(
            Tone::Dim,
            "moving your partitions, before any operating system starts".to_string(),
        ),
        Line::Rule,
        Line::Progress {
            filled: ((share * BAR_CELLS as f64).round() as usize).min(BAR_CELLS),
            percent: (share * 100.0).floor() as u64,
        },
        Line::Text(
            Tone::Body,
            format!(
                "{} of {} moved",
                format_bytes(moved),
                format_bytes(run.total_bytes)
            ),
        ),
        Line::Text(
            Tone::Dim,
            if done {
                format!(
                    "all {} chunks copied, flushed, verified, journaled",
                    run.total_chunks
                )
            } else {
                format!(
                    "chunk {} of {} copied, flushed, verified, journaled",
                    run.chunk, run.total_chunks
                )
            },
        ),
        Line::Rule,
    ];
    if done {
        lines.push(Line::Text(
            Tone::Green,
            "Done. Restarting into your system.".to_string(),
        ));
    } else {
        for (tone, text) in [
            (
                Tone::Green,
                "Power cut? Rude, but fine. Turn the computer back on.",
            ),
            (
                Tone::Green,
                "Kitten resumes from the last verified chunk. No drama.",
            ),
            (
                Tone::Amber,
                "Do not boot Windows from the firmware menu. A half-moved disk",
            ),
            (Tone::Amber, "is not something it handles gracefully."),
        ] {
            lines.push(Line::Text(tone, text.to_string()));
        }
    }
    lines
}

pub fn show(ui: &mut Ui, run: &RecoveryRun) {
    draw(ui, lines(run));
}

fn draw(ui: &mut Ui, lines: Vec<Line>) {
    let width = ui.available_width().min(theme::layout::CONSOLE_MAX);
    ui.allocate_ui(Vec2::new(width, 0.0), |ui| {
        let frame = eframe::egui::Frame::new()
            .fill(GROUND)
            .stroke(Stroke::new(1.0, theme::RULE))
            .corner_radius(4)
            .inner_margin(Margin {
                left: 27,
                right: 27,
                top: 22,
                bottom: 24,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(MIN_HEIGHT - 46.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                for line in &lines {
                    paint_line(ui, line);
                }
            });
        glow(ui, frame.response.rect);
    });
}

fn paint_line(ui: &mut Ui, line: &Line) {
    let row = 15.0 * 1.7;
    match line {
        Line::Rule => {
            ui.add_space(9.0);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
            ui.painter()
                .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, RULE));
            ui.add_space(9.0);
        }
        Line::Text(tone, text) => {
            let bold = *tone == Tone::Bright;
            let format = text_format(bold, colour(*tone), row);
            let mut job = LayoutJob::single_section(text.clone(), format);
            job.wrap.max_width = ui.available_width();
            let galley = ui.painter().layout_job(job);
            widgets::show_galley(ui, galley);
        }
        Line::Progress { filled, percent } => {
            let mut job = LayoutJob::default();
            job.append(&"█".repeat(*filled), 0.0, text_format(false, GREEN, row));
            job.append(
                &"░".repeat(BAR_CELLS - filled),
                0.0,
                text_format(false, DIM, row),
            );
            job.append(
                &format!("  {percent}%"),
                0.0,
                text_format(true, BRIGHT, row),
            );
            let galley = ui.painter().layout_job(job);
            widgets::show_galley(ui, galley);
        }
    }
}

fn text_format(bold: bool, colour: Color32, line_height: f32) -> eframe::egui::TextFormat {
    let mut format = widgets::format(Face::Console { bold }, theme::size::CONSOLE, colour);
    format.line_height = Some(line_height);
    format
}

fn colour(tone: Tone) -> Color32 {
    match tone {
        Tone::Body => BODY,
        Tone::Dim => DIM,
        Tone::Bright => BRIGHT,
        Tone::Green => GREEN,
        Tone::Amber => AMBER,
    }
}

/// A very faint green glow inside the edge, like phosphor.
fn glow(ui: &Ui, rect: Rect) {
    let painter = ui.painter().with_clip_rect(rect.shrink(1.0));
    for step in 0..18 {
        let inset = 1.0 + step as f32 * 5.0;
        let share = 0.03 * (1.0 - step as f32 / 18.0) / 6.0;
        painter.rect_stroke(
            rect.shrink(inset),
            CornerRadius::same(4),
            Stroke::new(5.0, theme::alpha(GREEN, share)),
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|line| match line {
                Line::Text(_, text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_running_console_warns_about_booting_windows() {
        let mut run = RecoveryRun::new(90 << 30);
        run.start(Duration::ZERO);
        run.advance(Duration::from_millis(2_000));
        let lines = lines(&run);
        assert!(lines.contains(&Line::Progress {
            filled: 3,
            percent: 7
        }));
        let texts = texts(&lines);
        assert!(texts.iter().any(|t| t.starts_with("chunk 110 of 1440")));
        assert!(texts.iter().any(|t| t.starts_with("Do not boot Windows")));
    }

    #[test]
    fn a_finished_console_says_done_and_counts_every_chunk() {
        let mut run = RecoveryRun::new(90 << 30);
        run.start(Duration::ZERO);
        run.advance(Duration::from_secs(60));
        let texts = texts(&lines(&run));
        assert!(texts.contains(&"Done. Restarting into your system.".to_string()));
        assert!(
            texts.contains(&"all 1440 chunks copied, flushed, verified, journaled".to_string())
        );
    }

    #[test]
    fn the_screen_is_black_when_the_power_first_goes() {
        let mut run = RecoveryRun::new(90 << 30);
        run.start(Duration::ZERO);
        run.advance(Duration::from_millis(500));
        run.cut_power(Duration::from_millis(500));
        run.advance(Duration::from_millis(600));
        assert!(lines(&run).is_empty());
    }
}
