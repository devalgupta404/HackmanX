//! The "Migrate to new hardware" goal: replicate this machine onto another, cut over with almost no
//! downtime, and finish with a signed receipt. Built from `bootshift-migrate-core`'s own types and
//! drawn with Kitten's theme, so it is one app, not a bolt-on. The demo backend converges a
//! migration and then commits; the real run wires into the same view.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Vec2, pos2};

use bootshift_migrate_core::{Advice, ConvergenceModel, PassKind, PassStats, Receipt, merkle_root};

use crate::app::Action;
use crate::theme::{self, Face, Weight};
use crate::widgets;

const MIB: u64 = 1 << 20;
/// The link rate the demo migration runs at (bytes per second).
const RATE: u64 = 500 * MIB;

pub struct MigrateView {
    model: ConvergenceModel,
    passes: Vec<PassStats>,
    advice: Advice,
    dirty: u64,
    receipt: Option<Receipt>,
    next_tick: f64,
    /// Set when the app drives a real migration between two machines. On cutover the view writes
    /// this "go" file; a host runner performs the migration and drops the receiver's signed
    /// receipt at `receipt_path`, which the view polls for. Absent in the standalone preview.
    go_path: Option<PathBuf>,
    receipt_path: Option<PathBuf>,
    /// Whether cutover has been asked for and the real run is in flight.
    requested: bool,
}

impl MigrateView {
    pub fn new() -> Self {
        MigrateView {
            model: ConvergenceModel::new(Duration::from_secs(5), Duration::from_millis(200), false),
            passes: Vec::new(),
            advice: Advice::Continue,
            dirty: 4096 * MIB,
            receipt: None,
            next_tick: 0.0,
            go_path: std::env::var_os("BOOTSHIFT_MIGRATE_GO").map(PathBuf::from),
            receipt_path: std::env::var_os("BOOTSHIFT_MIGRATE_RECEIPT").map(PathBuf::from),
            requested: false,
        }
    }

    pub fn reset(&mut self) {
        *self = MigrateView::new();
    }

    /// Whether this view drives a real migration rather than the standalone preview animation.
    fn real(&self) -> bool {
        self.go_path.is_some()
    }

    /// Whether the migration is still moving, so the window keeps repainting.
    pub fn animating(&self) -> bool {
        self.receipt.is_none()
    }

    /// Whether the cutover has happened and a signed receipt exists.
    #[cfg(test)]
    pub fn committed(&self) -> bool {
        self.receipt.is_some()
    }

    /// Advance the preview one pass every so often: the dirty set shrinks as it converges. The
    /// real migration moves data on the two machines instead, so it skips this animation.
    pub fn tick(&mut self, now: f64) {
        if self.real() || self.receipt.is_some() || matches!(self.advice, Advice::Ready { .. }) {
            return;
        }
        if self.next_tick == 0.0 {
            self.next_tick = now + 0.45;
            return;
        }
        if now < self.next_tick {
            return;
        }
        self.next_tick = now + 0.45;
        let sent = self.dirty.max(64 * MIB);
        self.dirty = (self.dirty as f64 * 0.45) as u64;
        let stats = PassStats {
            kind: if self.passes.is_empty() {
                PassKind::Bulk
            } else {
                PassKind::Delta
            },
            sent_bytes: sent,
            duration: Duration::from_millis(500),
            dirty_at_end: self.dirty,
            rate: RATE,
        };
        self.model.record(stats);
        self.passes.push(stats);
        self.advice = self.model.advise();
    }

    pub fn cutover(&mut self) {
        // Real run: signal the host runner and wait for the receiver's own signed receipt.
        if let Some(go) = self.go_path.clone() {
            self.requested = true;
            let _ = std::fs::write(&go, b"go");
            return;
        }
        let downtime = self
            .model
            .predicted_downtime()
            .unwrap_or_default()
            .as_millis() as u64;
        let root = merkle_root(&[[1u8; 32], [2u8; 32], [3u8; 32]]);
        let mut receipt = Receipt::new(
            "run-demo".to_string(),
            7,
            8 * 1024 * MIB,
            8 * 1024 * MIB,
            1 << 20,
            root,
            root,
            "2026-10-27T00:00:00Z".to_string(),
            "2026-10-27T00:00:05Z".to_string(),
            downtime,
        );
        receipt.sign_with_seed(&[0x5a; 32]);
        self.receipt = Some(receipt);
    }

    /// Real run: pick up the receiver's signed receipt once the host runner has written it.
    pub fn poll(&mut self) {
        if !self.requested || self.receipt.is_some() {
            return;
        }
        let Some(path) = &self.receipt_path else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        if let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes) {
            self.receipt = Some(receipt);
        }
    }
}

impl Default for MigrateView {
    fn default() -> Self {
        MigrateView::new()
    }
}

/// Draws the whole screen into the work area.
pub fn show(ui: &mut egui::Ui, view: &MigrateView, actions: &mut Vec<Action>, narrow: bool) {
    if back_link(ui).clicked() {
        actions.push(Action::Show(crate::app::Screen::Home));
    }
    ui.add_space(10.0);
    title(ui, "Migrate to new hardware", narrow);
    lede(
        ui,
        "This machine is replicated onto the new one while it keeps running. When the copy is \
         caught up, a brief freeze cuts over, and a signed receipt proves the target is identical.",
    );

    if view.real() {
        show_real(ui, view, actions);
    } else {
        show_preview(ui, view, actions);
    }

    if let Some(receipt) = &view.receipt {
        ui.add_space(22.0);
        receipt_card(ui, receipt);
    }
}

/// The real run: a readiness card, then a button that performs the migration between the two
/// machines, then an in-flight note until the receiver's signed receipt arrives.
fn show_real(ui: &mut egui::Ui, view: &MigrateView, actions: &mut Vec<Action>) {
    let (headline, note, accent) = if view.receipt.is_some() {
        (
            "Migration committed",
            "The new machine holds a byte-for-byte copy, proven by the receipt below.",
            theme::VERIFIED,
        )
    } else if view.requested {
        (
            "Cutting over",
            "Freezing the source, draining the last writes, and signing the receipt on the new \
             machine. This takes a few seconds.",
            theme::SIGNAL,
        )
    } else {
        (
            "Ready to replicate",
            "The new machine is waiting. Start the migration to copy this machine onto it and cut \
             over with a signed, verifiable receipt.",
            theme::VERIFIED,
        )
    };
    status_card(
        ui,
        headline,
        note,
        accent,
        view.requested && view.receipt.is_none(),
    );
    ui.add_space(22.0);

    let can_start = !view.requested && view.receipt.is_none();
    if view.receipt.is_none()
        && widgets::button(ui, "Start migration", widgets::Kind::Primary, can_start).clicked()
    {
        actions.push(Action::MigrateCutover);
    }
}

/// The standalone preview: the convergence animation driven by the demo model.
fn show_preview(ui: &mut egui::Ui, view: &MigrateView, actions: &mut Vec<Action>) {
    forecast_card(ui, view);
    ui.add_space(18.0);
    churn(ui, view);
    ui.add_space(22.0);

    let ready = matches!(view.advice, Advice::Ready { .. }) && view.receipt.is_none();
    if widgets::button(ui, "Cut over now", widgets::Kind::Primary, ready).clicked() {
        actions.push(Action::MigrateCutover);
    }
}

/// A headline-and-note card tinted by `accent`; shows a moving bar while `busy`.
fn status_card(ui: &mut egui::Ui, headline: &str, note: &str, accent: Color32, busy: bool) {
    let width = ui.available_width().min(640.0);
    let title_g = widgets::galley(ui, headline, Face::Sans(Weight::SemiBold), 16.0, theme::INK);
    let note_g = widgets::wrapped(
        ui,
        note,
        Face::Sans(Weight::Regular),
        13.0,
        theme::DIM,
        width - 58.0,
        1.4,
    );
    let bar_room = if busy { 18.0 } else { 0.0 };
    let height = 24.0 + title_g.size().y + 6.0 + note_g.size().y + bar_room + 24.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(10),
        theme::mix(accent, 0.05, theme::DECK_HIGH),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::alpha(accent, 0.45)),
        StrokeKind::Inside,
    );
    let dot = pos2(
        rect.left() + 26.0,
        rect.top() + 26.0 + title_g.size().y / 2.0,
    );
    painter.circle_filled(dot, 5.0, accent);
    let text_x = rect.left() + 42.0;
    widgets::text_at(
        painter,
        egui::Align2::LEFT_TOP,
        pos2(text_x, rect.top() + 24.0),
        title_g,
    );
    let note_y = rect.top() + 24.0 + 22.0;
    widgets::text_at(
        painter,
        egui::Align2::LEFT_TOP,
        pos2(text_x, note_y),
        note_g.clone(),
    );
    if busy {
        let track_y = note_y + note_g.size().y + 10.0;
        let track = Rect::from_min_size(pos2(text_x, track_y), Vec2::new(width - 68.0, 4.0));
        painter.rect_filled(track, CornerRadius::same(2), theme::BAR_GROUND);
        // A short lozenge sweeps across to show the run is alive without faking a percentage.
        let t = ui.input(|i| i.time).rem_euclid(1.4) / 1.4;
        let sweep = track.width() * 0.26;
        let x = track.left() + (track.width() + sweep) * t as f32 - sweep;
        let lo = x.max(track.left());
        let hi = (x + sweep).min(track.right());
        if hi > lo {
            let lozenge = Rect::from_min_max(pos2(lo, track_y), pos2(hi, track_y + 4.0));
            painter.rect_filled(lozenge, CornerRadius::same(2), accent);
        }
        ui.ctx().request_repaint();
    }
}

fn back_link(ui: &mut egui::Ui) -> egui::Response {
    let galley = widgets::galley(
        ui,
        "\u{2190}  Back",
        Face::Sans(Weight::Medium),
        13.0,
        theme::DIM,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    let colour = if response.hovered() {
        theme::INK
    } else {
        theme::DIM
    };
    let label = widgets::galley(
        ui,
        "\u{2190}  Back",
        Face::Sans(Weight::Medium),
        13.0,
        colour,
    );
    widgets::text_at(ui.painter(), egui::Align2::LEFT_TOP, rect.left_top(), label);
    response
}

fn title(ui: &mut egui::Ui, text: &str, narrow: bool) {
    let px = if narrow {
        theme::size::TITLE_NARROW
    } else {
        theme::size::TITLE
    };
    let galley = widgets::galley(ui, text, Face::Sans(Weight::SemiBold), px, theme::INK);
    widgets::show_galley(ui, galley);
}

fn lede(ui: &mut egui::Ui, text: &str) {
    ui.add_space(7.0);
    let width = ui.available_width().min(660.0);
    widgets::paragraph(ui, text, theme::size::LEDE, theme::DIM, width, 1.5);
    ui.add_space(24.0);
}

/// The one-line forecast, in a card tinted by how the migration is going.
fn forecast_card(ui: &mut egui::Ui, view: &MigrateView) {
    let width = ui.available_width().min(640.0);
    let (headline, note, accent) = forecast(view);
    let title_g = widgets::galley(
        ui,
        &headline,
        Face::Sans(Weight::SemiBold),
        16.0,
        theme::INK,
    );
    let note_g = widgets::wrapped(
        ui,
        &note,
        Face::Sans(Weight::Regular),
        13.0,
        theme::DIM,
        width - 58.0,
        1.4,
    );
    let height = 24.0 + title_g.size().y + 6.0 + note_g.size().y + 24.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(10),
        theme::mix(accent, 0.05, theme::DECK_HIGH),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::alpha(accent, 0.45)),
        StrokeKind::Inside,
    );
    let dot = pos2(
        rect.left() + 26.0,
        rect.top() + 26.0 + title_g.size().y / 2.0,
    );
    painter.circle_filled(dot, 5.0, accent);
    let text_x = rect.left() + 42.0;
    widgets::text_at(
        painter,
        egui::Align2::LEFT_TOP,
        pos2(text_x, rect.top() + 24.0),
        title_g,
    );
    widgets::text_at(
        painter,
        egui::Align2::LEFT_TOP,
        pos2(text_x, rect.top() + 24.0 + 22.0),
        note_g,
    );
}

fn forecast(view: &MigrateView) -> (String, String, Color32) {
    match view.advice {
        Advice::Ready { predicted_downtime } => (
            "Ready to cut over".to_string(),
            format!(
                "The copy is caught up. Cutting over now would cost about {} ms of data downtime.",
                predicted_downtime.as_millis()
            ),
            theme::VERIFIED,
        ),
        Advice::Continue => (
            "Catching up".to_string(),
            format!(
                "{} MiB still changing on the source; each pass sends only what moved.",
                view.dirty / MIB
            ),
            theme::SIGNAL,
        ),
        Advice::OfferThrottle { .. } | Advice::OfferMirror => (
            "High churn".to_string(),
            "The source is changing faster than the link. Throttle the busiest writers, or switch \
             to mirror mode."
                .to_string(),
            theme::SIGNAL_DIM,
        ),
        Advice::CannotConverge { .. } => (
            "Cannot converge".to_string(),
            "The churn is too high for this link.".to_string(),
            theme::STOP,
        ),
    }
}

/// Churn per pass: a shrinking row of bars, labelled, in Kitten's bar palette.
fn churn(ui: &mut egui::Ui, view: &MigrateView) {
    let caption = widgets::galley(
        ui,
        "CHURN PER PASS",
        Face::Mono(Weight::Medium),
        10.0,
        theme::FAINT,
    );
    widgets::show_galley(ui, caption);
    ui.add_space(8.0);

    let width = ui.available_width().min(640.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 108.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), theme::BAR_GROUND);
    if view.passes.is_empty() {
        let hint = widgets::galley(
            ui,
            "waiting for the first pass\u{2026}",
            Face::Sans(Weight::Regular),
            12.0,
            theme::FAINT,
        );
        widgets::text_at(painter, egui::Align2::CENTER_CENTER, rect.center(), hint);
        return;
    }
    let max = view
        .passes
        .iter()
        .map(|p| p.dirty_at_end)
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let slot = rect.width() / view.passes.len() as f32;
    for (index, pass) in view.passes.iter().enumerate() {
        let frac = (pass.dirty_at_end as f32 / max).clamp(0.02, 1.0);
        let bar_h = frac * (rect.height() - 20.0);
        let x = rect.left() + index as f32 * slot + slot * 0.18;
        let w = slot * 0.64;
        let bar = Rect::from_min_size(pos2(x, rect.bottom() - 10.0 - bar_h), Vec2::new(w, bar_h));
        // last bar is the freeze/cutover point: paint it green.
        let last = index + 1 == view.passes.len() && matches!(view.advice, Advice::Ready { .. });
        let colour = if last {
            theme::VERIFIED
        } else {
            theme::mix(theme::SIGNAL, 0.85, theme::DECK)
        };
        painter.rect_filled(bar, CornerRadius::same(3), colour);
    }
}

/// The signed receipt, styled like the Done screen's result.
fn receipt_card(ui: &mut egui::Ui, receipt: &Receipt) {
    let width = ui.available_width().min(640.0);
    let lines = [
        ("Target is identical to the source", receipt.roots_match()),
        ("Receipt signature is valid", receipt.verify()),
    ];
    let detail = format!(
        "Data downtime {} ms  \u{2022}  source root {}  \u{2022}  signed by {}",
        receipt.data_downtime_ms,
        hex6(&receipt.source_root),
        hex6(&receipt.receiver_key),
    );
    let head_g = widgets::galley(
        ui,
        "Migration committed",
        Face::Sans(Weight::SemiBold),
        17.0,
        theme::INK,
    );
    let detail_g = widgets::wrapped(
        ui,
        &detail,
        Face::Mono(Weight::Regular),
        11.5,
        theme::DIM,
        width - 48.0,
        1.5,
    );
    let height = 22.0
        + head_g.size().y
        + 16.0
        + (lines.len() as f32 * 24.0)
        + 12.0
        + detail_g.size().y
        + 22.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(10),
        theme::mix(theme::VERIFIED, 0.05, theme::DECK_HIGH),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::alpha(theme::VERIFIED, 0.4)),
        StrokeKind::Inside,
    );

    let left = rect.left() + 24.0;
    let mut y = rect.top() + 22.0;
    widgets::text_at(
        painter,
        egui::Align2::LEFT_TOP,
        pos2(left, y),
        head_g.clone(),
    );
    y += head_g.size().y + 16.0;
    for (text, ok) in lines {
        let mark = pos2(left + 8.0, y + 8.0);
        if ok {
            widgets::tick(painter, mark, 11.0, theme::VERIFIED, 2.0);
        } else {
            painter.circle_stroke(mark, 6.0, Stroke::new(2.0, theme::STOP));
        }
        let g = widgets::galley(ui, text, Face::Sans(Weight::Medium), 13.5, theme::INK);
        widgets::text_at(painter, egui::Align2::LEFT_TOP, pos2(left + 24.0, y), g);
        y += 24.0;
    }
    y += 12.0;
    widgets::text_at(painter, egui::Align2::LEFT_TOP, pos2(left, y), detail_g);
}

fn hex6(bytes: &[u8; 32]) -> String {
    bytes[..6].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_mode_writes_go_then_loads_the_receivers_receipt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let go = dir.path().join("go");
        let receipt_path = dir.path().join("receipt.json");
        let mut view = MigrateView::new();
        view.go_path = Some(go.clone());
        view.receipt_path = Some(receipt_path.clone());
        assert!(view.real());

        // Cutover signals the runner and waits; it does not fabricate a receipt.
        view.cutover();
        assert!(go.exists(), "cutover should write the go file");
        assert!(view.requested);
        assert!(view.receipt.is_none());

        // Polling before the receiver has written anything is a no-op.
        view.poll();
        assert!(view.receipt.is_none());

        // Once the receiver's signed receipt is on disk, poll loads it verbatim.
        let root = merkle_root(&[[1u8; 32], [2u8; 32]]);
        let mut receipt = Receipt::new(
            "run".to_string(),
            1,
            1024,
            1024,
            1 << 20,
            root,
            root,
            "a".to_string(),
            "b".to_string(),
            7,
        );
        receipt.sign_with_seed(&[3u8; 32]);
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).expect("encode"))
            .expect("write receipt");
        view.poll();
        assert!(
            view.committed(),
            "poll should load the receipt once it exists"
        );
        assert!(
            view.receipt
                .as_ref()
                .is_some_and(|r| r.roots_match() && r.verify())
        );
    }
}
