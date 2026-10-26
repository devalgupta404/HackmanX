//! The five screens, shown one at a time. Each is a header (title and lede), then its content;
//! the buttons live in the action bar.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Margin, Pos2, Rect, Role, Sense, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, text::LayoutJob,
};

use crate::app::{Action, App, Goal, RowState, Screen};
use crate::disk_bar::DiskBar;
use crate::grid::{self, Column};
use crate::theme::{self, Face, Weight, layout, size};
use crate::widgets::{self, Kind};
use crate::{console, gutter, shell, table};

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>, narrow: bool) {
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    match app.screen {
        Screen::Home => home(ui, app, actions, narrow),
        Screen::Arrange => arrange(ui, app, actions, narrow),
        Screen::Plan => plan(ui, app, narrow),
        Screen::Checks => checks(ui, app, actions, narrow),
        Screen::Restart => restart(ui, app, narrow),
        Screen::Done => done(ui, app, narrow),
    }
}

fn header(ui: &mut Ui, title: &str, lede: &str, narrow: bool) {
    title_text(ui, title, narrow);
    lede_text(ui, lede);
}

fn title_text(ui: &mut Ui, title: &str, narrow: bool) {
    let px = if narrow {
        size::TITLE_NARROW
    } else {
        size::TITLE
    };
    let mut format = widgets::format(Face::Sans(Weight::SemiBold), px, theme::INK);
    format.extra_letter_spacing = -0.025 * px;
    format.line_height = Some(px * 1.15);
    let galley = ui
        .painter()
        .layout_job(LayoutJob::single_section(title.to_string(), format));
    widgets::show_galley(ui, galley);
}

fn lede_text(ui: &mut Ui, lede: &str) {
    ui.add_space(7.0);
    let width = ui.available_width().min(layout::LEDE_MAX);
    widgets::paragraph(ui, lede, size::LEDE, theme::DIM, width, 1.5);
    ui.add_space(24.0);
}

/* ---------- arrange ---------- */

fn arrange(ui: &mut Ui, app: &App, actions: &mut Vec<Action>, narrow: bool) {
    header(
        ui,
        "Arrange the disk",
        "Drag a partition to move it. Drag an edge to resize it. Nothing is written until you apply.",
        narrow,
    );
    if !app.disk_problems.is_empty() {
        widgets::refusals(ui, &app.disk_problems);
        return;
    }
    let Some(probed) = &app.probed else {
        return;
    };
    let layout = &probed.layout;
    widgets::panel(ui, Margin::symmetric(19, 17), |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        bar_caption(ui, "Now", "as the disk is today");
        let before = DiskBar {
            id: "before",
            layout,
            target: &probed.target,
            selected: app.selected,
            editable: false,
        }
        .show(ui);
        gutter::show(ui, layout, &app.target);
        bar_caption(ui, "After the restart", "drag to change");
        let after = DiskBar {
            id: "after",
            layout,
            target: &app.target,
            selected: app.selected,
            editable: true,
        }
        .show(ui);
        for output in [before, after] {
            if let Some(guid) = output.picked {
                actions.push(Action::Pick(guid));
            }
            if let Some((guid, extent)) = output.edit {
                actions.push(Action::Edit(guid, extent));
            }
        }
    });
    ui.add_space(19.0);
    if let Some(guid) = table::show(ui, layout, &app.target, app.selected) {
        actions.push(Action::Pick(guid));
    }
}

fn bar_caption(ui: &mut Ui, caption: &str, note: &str) {
    let width = ui.available_width();
    let caption = widgets::galley(ui, caption, Face::Sans(Weight::SemiBold), 11.7, theme::DIM);
    let note = widgets::galley(ui, note, theme::MONO, 10.2, theme::FAINT);
    let height = caption.size().y.max(note.size().y);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    let baseline = rect.bottom() - 2.0;
    widgets::text_at(
        painter,
        Align2::LEFT_BOTTOM,
        Pos2::new(rect.left(), baseline + 2.0),
        caption,
    );
    widgets::text_at(
        painter,
        Align2::RIGHT_BOTTOM,
        Pos2::new(rect.right(), baseline + 1.0),
        note,
    );
    ui.add_space(7.0);
}

/* ---------- plan ---------- */

fn plan(ui: &mut Ui, app: &App, narrow: bool) {
    header(
        ui,
        "The plan",
        "Every step the recovery system will take, in the order it takes them.",
        narrow,
    );
    let steps: Vec<String> = match (&app.plan, &app.probed) {
        (Some(plan), Some(probed)) => plan
            .operations
            .iter()
            .map(|op| op.describe(&probed.layout))
            .collect(),
        _ => Vec::new(),
    };
    widgets::panel(ui, Margin::ZERO, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        if steps.is_empty() {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(15, 22))
                .show(ui, |ui| {
                    let width = ui.available_width();
                    widgets::paragraph(
                        ui,
                        "Nothing to do yet. Move a partition on the previous screen.",
                        13.2,
                        theme::FAINT,
                        width,
                        1.5,
                    );
                });
        }
        for (index, step) in steps.iter().enumerate() {
            plan_step(ui, index, step, index + 1 == steps.len());
        }
    });
    if !app.plan_problems.is_empty() {
        ui.add_space(15.0);
        widgets::refusals(ui, &app.plan_problems);
    }
}

/// Numbering is right here because this really is a sequence; nowhere else uses numbers.
fn plan_step(ui: &mut Ui, index: usize, text: &str, last: bool) {
    let width = ui.available_width();
    let text_width = width - 15.0 - 24.0 - 13.5 - 15.0;
    let sentence = widgets::wrapped(ui, text, theme::SANS, 13.5, theme::INK, text_width, 1.45);
    let height = sentence.size().y.max(24.0) + 22.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    let circle = Rect::from_min_size(
        Pos2::new(rect.left() + 15.0, rect.center().y - 12.0),
        Vec2::splat(24.0),
    );
    painter.circle_stroke(circle.center(), 11.5, Stroke::new(1.0, theme::RULE));
    let number = widgets::galley(
        ui,
        (index + 1).to_string(),
        theme::MONO,
        10.8,
        theme::SIGNAL,
    );
    widgets::text_at(painter, Align2::CENTER_CENTER, circle.center(), number);
    widgets::text_at(
        painter,
        Align2::LEFT_CENTER,
        Pos2::new(circle.right() + 13.5, rect.center().y),
        sentence,
    );
    if !last {
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, theme::RULE_SOFT),
        );
    }
}

/* ---------- checks ---------- */

const CHECK_COLUMNS: [Column; 3] = [
    Column::Fixed(16.5),
    Column::Flex {
        min: 135.0,
        fr: 0.9,
    },
    Column::Flex { min: 0.0, fr: 1.6 },
];

fn checks(ui: &mut Ui, app: &App, actions: &mut Vec<Action>, narrow: bool) {
    header(
        ui,
        "Safety checks",
        "All of them pass, or BootShift writes nothing at all.",
        narrow,
    );
    if !app.checks.is_empty() {
        widgets::panel(ui, Margin::ZERO, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for index in 0..app.checks.len() {
                check_row(ui, app, index);
            }
        });
    }
    if !app.check_problems.is_empty() {
        ui.add_space(15.0);
        widgets::refusals(ui, &app.check_problems);
    }
    ui.add_space(15.0);
    widgets::panel(ui, Margin::symmetric(15, 13), |ui| gate(ui, app, actions));
}

/// The backup checkbox on the left and the save button on the right, stacked when the two
/// don't fit side by side.
fn gate(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    const SAVE: &str = "Save the partition table";
    let text = "My important files are backed up somewhere off this disk";
    let checkbox_width = 16.0
        + 9.0
        + widgets::galley(ui, text, theme::SANS, 13.2, theme::INK)
            .size()
            .x;
    let button_width = widgets::galley(
        ui,
        SAVE,
        Face::Sans(Weight::Medium),
        size::BUTTON,
        theme::INK,
    )
    .size()
    .x + 32.0;
    let side_by_side = checkbox_width + 22.0 + button_width <= ui.available_width();
    let layout = if side_by_side {
        egui::Layout::left_to_right(egui::Align::Center)
    } else {
        egui::Layout::top_down(egui::Align::Min)
    };
    let height = if side_by_side { 30.0 } else { 70.0 };
    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), height), layout, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(22.0, 10.0);
        if shell::checkbox_row(ui, app.backed_up, text).clicked() {
            actions.push(Action::ToggleBackedUp);
        }
        let save = |ui: &mut Ui| widgets::button(ui, SAVE, Kind::Secondary, app.probed.is_some());
        let clicked = if side_by_side {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), save)
                .inner
                .clicked()
        } else {
            save(ui).clicked()
        };
        if clicked {
            actions.push(Action::OpenSave);
        }
    });
}

fn check_row(ui: &mut Ui, app: &App, index: usize) {
    let Some(check) = app.checks.get(index) else {
        return;
    };
    let state = app.row_state(index);
    let width = ui.available_width();
    let cols = grid::columns(width - 30.0, 15.0, &CHECK_COLUMNS);
    let resolved = matches!(state, RowState::Passed | RowState::Failed);
    let name_colour = if resolved { theme::INK } else { theme::FAINT };
    let detail_colour = if state == RowState::Failed {
        theme::STOP
    } else {
        theme::DIM
    };
    let name = widgets::wrapped(
        ui,
        &check.name,
        theme::SANS,
        12.9,
        name_colour,
        cols[1].1,
        1.4,
    );
    let detail_text = if resolved { check.detail.as_str() } else { "" };
    let detail = widgets::wrapped(
        ui,
        detail_text,
        theme::MONO,
        11.1,
        detail_colour,
        cols[2].1,
        1.4,
    );
    let height = name.size().y.max(detail.size().y).max(16.5) + 16.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    let x = |column: usize| rect.left() + 15.0 + cols[column].0;
    let mark = Rect::from_center_size(Pos2::new(x(0) + 8.25, rect.center().y), Vec2::splat(16.5));
    paint_mark(ui, painter, mark, state);
    widgets::text_at(
        painter,
        Align2::LEFT_CENTER,
        Pos2::new(x(1), rect.center().y),
        name,
    );
    widgets::text_at(
        painter,
        Align2::LEFT_CENTER,
        Pos2::new(x(2), rect.center().y),
        detail,
    );
    if index + 1 < app.checks.len() {
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, theme::RULE_SOFT),
        );
    }
}

fn paint_mark(ui: &Ui, painter: &egui::Painter, rect: Rect, state: RowState) {
    let centre = rect.center();
    let radius = rect.width() / 2.0;
    match state {
        RowState::Waiting => {
            painter.circle_stroke(centre, radius - 0.5, Stroke::new(1.0, theme::RULE));
        }
        RowState::Running => {
            // Pulses 100% -> 35% -> 100% every 0.7s, so a slow check reads as working.
            let opacity = if crate::motion::reduced() {
                1.0
            } else {
                let phase = (ui.input(|i| i.time) % 0.7) / 0.7;
                let wave = (phase * std::f64::consts::TAU).cos() * 0.5 + 0.5;
                (0.35 + 0.65 * wave) as f32
            };
            ui.ctx().request_repaint();
            let amber = theme::alpha(theme::SIGNAL, opacity);
            painter.circle_stroke(centre, radius - 0.5, Stroke::new(1.0, amber));
            painter.circle_filled(centre, 2.0, amber);
        }
        RowState::Passed | RowState::Failed => {
            let passed = state == RowState::Passed;
            let (colour, border) = if passed {
                (theme::VERIFIED, 0.45)
            } else {
                (theme::STOP, 0.5)
            };
            painter.circle_filled(centre, radius, theme::alpha(colour, 0.12));
            painter.circle_stroke(
                centre,
                radius - 0.5,
                Stroke::new(1.0, theme::alpha(colour, border)),
            );
            if passed {
                widgets::tick(painter, centre, 7.5, colour, 1.4);
            } else {
                widgets::cross(painter, centre, 6.0, colour, 1.4);
            }
        }
    }
}

/* ---------- home ---------- */

fn home(ui: &mut Ui, app: &App, actions: &mut Vec<Action>, narrow: bool) {
    header(
        ui,
        "What would you like to do?",
        "BootShift works out the safe way to do it, and shows you before anything changes.",
        narrow,
    );
    goal_card(ui, app, Goal::InstallOs, true, actions);
    ui.add_space(12.0);
    let middle = [Goal::GiveSpace, Goal::FreeSpace, Goal::MovePartition, Goal::CleanUp];
    if narrow {
        for goal in middle {
            goal_card(ui, app, goal, false, actions);
            ui.add_space(12.0);
        }
    } else {
        for pair in middle.chunks(2) {
            ui.columns(2, |cols| {
                for (i, &goal) in pair.iter().enumerate() {
                    goal_card(&mut cols[i], app, goal, false, actions);
                }
            });
            ui.add_space(12.0);
        }
    }
    goal_card(ui, app, Goal::DoItMyself, false, actions);
}

fn goal_colour(goal: Goal) -> Color32 {
    match goal {
        Goal::InstallOs => theme::SIGNAL,
        Goal::GiveSpace => Color32::from_rgb(0x38, 0xb2, 0xa8),
        Goal::FreeSpace => Color32::from_rgb(0x4d, 0x7e, 0xf2),
        Goal::MovePartition => Color32::from_rgb(0xb0, 0x7c, 0xf5),
        Goal::CleanUp | Goal::DoItMyself => theme::DIM,
    }
}

fn goal_card(ui: &mut Ui, app: &App, goal: Goal, featured: bool, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    let height = if featured { 98.0 } else { 86.0 };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let enabled = app.probed.is_some();
    let hovered = response.hovered() && enabled;

    // Resting, the card shows only its title, centred. On hover the title rises and the
    // description slides down and fades in, so there is less text until you reach for it.
    let reveal = ui
        .ctx()
        .animate_bool_with_time(ui.id().with(("goal-card", goal.title())), hovered, 0.13);

    // Lay the text out before borrowing the painter; the blurb fades with the reveal.
    let title = widgets::galley(ui, goal.title(), Face::Sans(Weight::SemiBold), 16.0, theme::INK);
    let chip = Rect::from_min_size(
        rect.left_top() + Vec2::new(16.0, (height - if featured { 46.0 } else { 42.0 }) / 2.0),
        Vec2::splat(if featured { 46.0 } else { 42.0 }),
    );
    let text_x = chip.right() + 15.0;
    let blurb = widgets::elided(
        ui,
        goal.blurb(),
        Face::Sans(Weight::Regular),
        13.0,
        theme::alpha(theme::DIM, reveal),
        rect.right() - text_x - 18.0,
    );
    let tag = match goal {
        Goal::InstallOs => Some(("MOST COMMON", theme::SIGNAL)),
        Goal::DoItMyself => Some(("ADVANCED", theme::FAINT)),
        _ => None,
    };
    let tag = tag.map(|(text, colour)| widgets::galley(ui, text, Face::Mono(Weight::Medium), 9.5, colour));

    let painter = ui.painter();
    let radius = CornerRadius::same(10);
    let bg = if featured {
        theme::mix(theme::SIGNAL, 0.06, theme::DECK_HIGH)
    } else {
        theme::DECK_HIGH
    };
    painter.rect_filled(rect, radius, bg);
    let border = if hovered {
        theme::RULE_HOVER
    } else if featured {
        theme::alpha(theme::SIGNAL, 0.4)
    } else {
        theme::RULE
    };
    painter.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);

    painter.rect_filled(chip, CornerRadius::same(9), theme::DECK);
    painter.rect_filled(chip.shrink(14.0), CornerRadius::same(4), goal_colour(goal));

    // The title slides from vertically centred (resting) up to just below the tag (hovered).
    let title_h = 20.0;
    let rest_y = rect.top() + (height - title_h) / 2.0;
    let raised_y = rect.top() + if featured { 24.0 } else { 20.0 };
    let title_y = egui::lerp(rest_y..=raised_y, reveal);
    widgets::text_at(painter, Align2::LEFT_TOP, Pos2::new(text_x, title_y), title);
    if reveal > 0.01 {
        let blurb_y = title_y + egui::lerp(16.0..=24.0, reveal);
        widgets::text_at(painter, Align2::LEFT_TOP, Pos2::new(text_x, blurb_y), blurb);
    }
    if let Some(galley) = tag {
        widgets::text_at(
            painter,
            Align2::LEFT_TOP,
            Pos2::new(text_x, rect.top() + if featured { 8.0 } else { 7.0 }),
            galley,
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, goal.title()));
    if response.clicked() && enabled {
        actions.push(Action::ChooseGoal(goal));
    }
}

/* ---------- restart ---------- */

fn restart(ui: &mut Ui, app: &App, narrow: bool) {
    if app.is_demo() {
        header(
            ui,
            "During the restart",
            "This is what the computer shows while BootShift works, before any operating system starts.",
            narrow,
        );
        if let Some(run) = &app.run {
            console::show(ui, run);
        }
        return;
    }

    // Real disk: the move happens in the recovery system on the next boot, on the machine's own
    // screen, not here. Say so plainly and show what that screen will look like.
    header(
        ui,
        "Ready to restart",
        "Everything is prepared. Restart this computer to apply the change.",
        narrow,
    );
    let width = ui.available_width().min(layout::LEDE_MAX);
    widgets::paragraph(
        ui,
        "The move runs in BootShift's recovery system, before any operating system starts, so it \
         appears on the machine's own screen, not in this window. If the power goes out, just turn \
         the computer back on; it resumes from the journal by itself. Reopen BootShift afterwards \
         to see the new layout.",
        size::LEDE,
        theme::DIM,
        width,
        1.5,
    );
}

/* ---------- done ---------- */

fn done(ui: &mut Ui, app: &App, narrow: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.5;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
        let painter = ui.painter();
        painter.circle_filled(rect.center(), 15.0, theme::alpha(theme::VERIFIED, 0.10));
        painter.circle_stroke(
            rect.center(),
            14.5,
            Stroke::new(1.0, theme::alpha(theme::VERIFIED, 0.45)),
        );
        widgets::tick(painter, rect.center(), 12.0, theme::VERIFIED, 2.0);
        title_text(ui, "Done", narrow);
    });
    let lede = if app.is_demo() {
        "Every moved byte was read back and verified. The power cut in the middle changed nothing."
    } else {
        "The disk matches the plan. Every moved byte was read back and verified."
    };
    lede_text(ui, lede);

    if let Some(outcome) = &app.outcome {
        let cuts = if outcome.power_cuts == 1 {
            "power cut survived"
        } else {
            "power cuts survived"
        };
        let time = if app.is_demo() {
            "simulated time"
        } else {
            "total time"
        };
        stats(
            ui,
            &[
                (
                    bootshift_core::units::format_bytes(outcome.bytes),
                    "data moved",
                ),
                (outcome.chunks.to_string(), "chunks verified"),
                (outcome.power_cuts.to_string(), cuts),
                (format!("{}s", outcome.seconds), time),
            ],
        );
    }
    ui.add_space(19.0);
    if let Some(probed) = &app.probed {
        widgets::panel(ui, Margin::symmetric(19, 17), |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            bar_caption(ui, "Your disk now", "verified byte for byte");
            DiskBar {
                id: "done",
                layout: &probed.layout,
                target: &app.target,
                selected: None,
                editable: false,
            }
            .show(ui);
        });
    }
    ui.add_space(15.0);
    let width = ui.available_width().min(layout::LEDE_MAX);
    widgets::paragraph(
        ui,
        "The full log and your old partition table stay on the EFI system partition until the next change.",
        12.6,
        theme::FAINT,
        width,
        1.5,
    );
}

/// Equal cells with 1px rules between them; they wrap onto more rows in a narrow window.
fn stats(ui: &mut Ui, cells: &[(String, &str)]) {
    let width = ui.available_width();
    let per_row = (((width + 1.0) / 151.0).floor() as usize).clamp(1, cells.len().max(1));
    let rows = cells.len().div_ceil(per_row);
    let cell_width = (width - 2.0 - (per_row - 1) as f32) / per_row as f32;
    let cell_height = 70.0;
    let height = rows as f32 * cell_height + (rows - 1) as f32 + 2.0;
    let (frame, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(frame, 4, theme::RULE);
    for (index, (value, key)) in cells.iter().enumerate() {
        let (row, column) = (index / per_row, index % per_row);
        let cell = Rect::from_min_size(
            Pos2::new(
                frame.left() + 1.0 + column as f32 * (cell_width + 1.0),
                frame.top() + 1.0 + row as f32 * (cell_height + 1.0),
            ),
            Vec2::new(cell_width, cell_height),
        );
        painter.rect_filled(cell, 0, theme::DECK);
        let mut format = widgets::format(Face::Mono(Weight::Medium), size::STAT, theme::INK);
        format.extra_letter_spacing = -0.03 * size::STAT;
        let value = ui
            .painter()
            .layout_job(LayoutJob::single_section(value.clone(), format));
        let value_rect = widgets::text_at(
            painter,
            Align2::LEFT_TOP,
            cell.left_top() + Vec2::new(16.5, 15.0),
            value,
        );
        let key = widgets::galley(ui, *key, theme::SANS, 11.4, theme::FAINT);
        widgets::text_at(
            painter,
            Align2::LEFT_TOP,
            Pos2::new(value_rect.left(), value_rect.bottom() + 2.0),
            key,
        );
    }
    painter.rect_stroke(frame, 4, Stroke::new(1.0, theme::RULE), StrokeKind::Inside);
}
