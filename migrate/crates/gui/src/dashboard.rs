//! The migration dashboard, built straight from `bootshift-migrate-core`'s types (plan 12.2): the
//! churn of each pass, the convergence forecast, a cutover button that lights up when a cutover is
//! cheap, and the signed receipt once it commits. The demo backend drives a migration that
//! converges and then commits, so the screens can be seen and tested without a live run.

use std::time::Duration;

use eframe::egui::{self, Color32, Rect, RichText, Sense, Vec2, pos2};

use bootshift_migrate_core::{Advice, ConvergenceModel, PassKind, PassStats, Receipt, merkle_root};

const MIB: u64 = 1 << 20;
/// The link rate the demo migration runs at (bytes per second).
const RATE: u64 = 500 * MIB;

pub struct App {
    model: ConvergenceModel,
    passes: Vec<PassStats>,
    advice: Advice,
    dirty_bytes: u64,
    receipt: Option<Receipt>,
    frame: u64,
    /// For an unattended recording: cut over by itself a moment after it is ready.
    autocut: bool,
    ready_frames: u64,
}

impl App {
    pub fn new() -> Self {
        App {
            model: ConvergenceModel::new(Duration::from_secs(5), Duration::from_millis(200), false),
            passes: Vec::new(),
            advice: Advice::Continue,
            dirty_bytes: 4096 * MIB,
            receipt: None,
            frame: 0,
            autocut: std::env::var_os("MIGRATE_GUI_AUTOCUT").is_some(),
            ready_frames: 0,
        }
    }

    #[allow(dead_code)]
    pub fn advice(&self) -> Advice {
        self.advice
    }

    #[allow(dead_code)]
    pub fn receipt(&self) -> Option<&Receipt> {
        self.receipt.as_ref()
    }

    /// Whether the demo still has work to show (so the window keeps repainting).
    pub fn animating(&self) -> bool {
        self.receipt.is_none()
    }

    /// Advance the demo one pass: the dirty set shrinks as the migration converges.
    fn step(&mut self) {
        if self.receipt.is_some() || matches!(self.advice, Advice::Ready { .. }) {
            return;
        }
        let sent = self.dirty_bytes.max(64 * MIB);
        self.dirty_bytes = (self.dirty_bytes as f64 * 0.42) as u64;
        let stats = PassStats {
            kind: if self.passes.is_empty() {
                PassKind::Bulk
            } else {
                PassKind::Delta
            },
            sent_bytes: sent,
            duration: Duration::from_millis(500),
            dirty_at_end: self.dirty_bytes,
            rate: RATE,
        };
        self.model.record(stats);
        self.passes.push(stats);
        self.advice = self.model.advise();
    }

    /// Cut over: the final pass makes the target equal the source, so the roots match and the
    /// receiver signs the receipt.
    fn cutover(&mut self) {
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

    pub fn render(&mut self, ui: &mut egui::Ui) {
        self.frame += 1;
        if self.receipt.is_none() && self.frame % 12 == 0 {
            self.step();
        }
        if self.autocut && self.receipt.is_none() && matches!(self.advice, Advice::Ready { .. }) {
            self.ready_frames += 1;
            if self.ready_frames > 48 {
                self.cutover();
            }
        }

        ui.heading("Kitten Migrate");
        ui.add_space(8.0);

        let (text, color) = forecast_line(&self.advice, self.dirty_bytes);
        ui.label(RichText::new(text).color(color).size(18.0));
        ui.add_space(14.0);

        ui.label(RichText::new("Churn per pass — data still changing").strong());
        self.chart(ui);
        ui.add_space(14.0);

        let ready = matches!(self.advice, Advice::Ready { .. }) && self.receipt.is_none();
        if ui
            .add_enabled(
                ready,
                egui::Button::new(RichText::new("Cut over now").size(16.0)),
            )
            .clicked()
        {
            self.cutover();
        }
        ui.add_space(14.0);

        if let Some(receipt) = &self.receipt {
            receipt_panel(ui, receipt);
        }
    }

    fn chart(&self, ui: &mut egui::Ui) {
        let desired = Vec2::new(ui.available_width().min(560.0), 120.0);
        let (response, painter) = ui.allocate_painter(desired, Sense::hover());
        let rect = response.rect;
        painter.rect_filled(rect, 4.0, Color32::from_gray(28));
        if self.passes.is_empty() {
            return;
        }
        let max = self
            .passes
            .iter()
            .map(|p| p.dirty_at_end)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let bar_width = rect.width() / self.passes.len() as f32;
        for (index, pass) in self.passes.iter().enumerate() {
            let height = (pass.dirty_at_end as f32 / max) * (rect.height() - 8.0);
            let x = rect.left() + index as f32 * bar_width + 2.0;
            let bar = Rect::from_min_size(
                pos2(x, rect.bottom() - height - 4.0),
                Vec2::new((bar_width - 4.0).max(1.0), height),
            );
            painter.rect_filled(bar, 2.0, Color32::from_rgb(90, 170, 120));
        }
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

fn forecast_line(advice: &Advice, dirty: u64) -> (String, Color32) {
    match advice {
        Advice::Ready { predicted_downtime } => (
            format!(
                "Ready to cut over — predicted data downtime {} ms",
                predicted_downtime.as_millis()
            ),
            Color32::from_rgb(120, 200, 140),
        ),
        Advice::Continue => (
            format!("Converging — {} MiB still changing", dirty / MIB),
            Color32::from_rgb(220, 200, 120),
        ),
        Advice::OfferThrottle { .. } => (
            "High churn — consider throttling the busiest writers".to_string(),
            Color32::from_rgb(230, 160, 90),
        ),
        Advice::OfferMirror => (
            "High churn — switch to mirror mode".to_string(),
            Color32::from_rgb(230, 160, 90),
        ),
        Advice::CannotConverge { .. } => (
            "Cannot converge at this churn".to_string(),
            Color32::from_rgb(230, 110, 110),
        ),
    }
}

fn receipt_panel(ui: &mut egui::Ui, receipt: &Receipt) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(
            RichText::new("Migration committed — signed receipt")
                .strong()
                .color(Color32::from_rgb(120, 200, 140)),
        );
        ui.label(format!(
            "Target equals source: {}",
            yes_no(receipt.roots_match())
        ));
        ui.label(format!("Signature valid: {}", yes_no(receipt.verify())));
        ui.label(format!("Data downtime: {} ms", receipt.data_downtime_ms));
        ui.label(format!("Source root: {}", hex8(&receipt.source_root)));
        ui.label(format!("Signed by: {}", hex8(&receipt.receiver_key)));
    });
}

fn yes_no(ok: bool) -> &'static str {
    if ok { "YES" } else { "NO" }
}

fn hex8(bytes: &[u8; 32]) -> String {
    bytes[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn the_demo_converges_then_shows_a_valid_signed_receipt() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(640.0, 760.0))
            .build_ui_state(|ui, app: &mut App| app.render(ui), App::new());

        // The demo advances a pass every few frames; run until it is ready to cut over.
        for _ in 0..400 {
            harness.run_steps(1);
            if matches!(harness.state().advice(), Advice::Ready { .. }) {
                break;
            }
        }
        assert!(
            matches!(harness.state().advice(), Advice::Ready { .. }),
            "the demo migration should converge to ready"
        );

        harness.get_by_label("Cut over now").click();
        harness.run_steps(2);

        let receipt = harness
            .state()
            .receipt()
            .expect("cutover should produce a receipt");
        assert!(receipt.verify(), "the receipt's signature verifies");
        assert!(receipt.roots_match(), "the target equals the source");
    }
}
