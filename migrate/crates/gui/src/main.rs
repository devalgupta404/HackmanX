//! The BootShift Migrate dashboard window (plan 12.2). It reads only `bootshift-migrate-core`'s
//! types and shows the migration: churn per pass, the convergence forecast, the cutover button, and
//! the signed receipt. The demo backend drives a converging migration so there is something to see.

mod dashboard;

use std::time::Duration;

use eframe::egui;

use dashboard::App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
        if self.animating() {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
    }
}

fn main() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BootShift Migrate")
            .with_app_id("org.bootshift.Migrate")
            .with_inner_size([620.0, 680.0])
            .with_min_inner_size([520.0, 560.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "BootShift Migrate",
        options,
        Box::new(|_context| Ok(Box::new(App::new()))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}
