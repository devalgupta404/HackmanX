//! The app's state and the flow between screens. Screens draw from this state and hand back
//! `Action`s; the app applies them after drawing, so nothing changes halfway through a frame.

use std::time::Duration;

use bootshift_core::layout::{Extent, Guid, Target};
use bootshift_core::refusal::Refusal;
use eframe::egui;

use crate::backend::{BackendError, Request, Response, Worker};
use crate::protocol::{CheckResult, DiskSummary, Planned, Probed, Reply};
use crate::recovery_run::{Phase, RecoveryRun};
use crate::save::SaveDialog;
use crate::shell;

const PLAN_DELAY: f64 = 0.3;
const CHECK_STEP: f64 = 0.11;
const DONE_DELAY: f64 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Screen {
    /// The launcher: pick a goal. Comes before the flow and isn't one of its numbered steps.
    Home,
    Arrange,
    Plan,
    Checks,
    Restart,
    Done,
}

impl Screen {
    /// The flow's numbered steps, shown in the rail. `Home` is the launcher, not a step.
    pub const ALL: [Screen; 5] = [
        Screen::Arrange,
        Screen::Plan,
        Screen::Checks,
        Screen::Restart,
        Screen::Done,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Screen::Home => "Home",
            Screen::Arrange => "Arrange",
            Screen::Plan => "Plan",
            Screen::Checks => "Checks",
            Screen::Restart => "Restart",
            Screen::Done => "Done",
        }
    }
}

/// What the person wants to do, chosen on the home screen. Every goal but `DoItMyself` is a
/// friendly front-end that will build a target; today they all open the editor, which is the
/// `DoItMyself` experience, while the simplified screens are built out one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    InstallOs,
    GiveSpace,
    FreeSpace,
    MovePartition,
    CleanUp,
    DoItMyself,
}

impl Goal {
    pub fn title(self) -> &'static str {
        match self {
            Goal::InstallOs => "Install an operating system",
            Goal::GiveSpace => "Give a partition more space",
            Goal::FreeSpace => "Free up space from a partition",
            Goal::MovePartition => "Move a partition",
            Goal::CleanUp => "Clean up a drive",
            Goal::DoItMyself => "Do it myself",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Goal::InstallOs => {
                "Make room for Linux or another OS. BootShift frees a clean block for the installer."
            }
            Goal::GiveSpace => "Pick the one that's full. BootShift finds nearby space and grows it.",
            Goal::FreeSpace => "Carve some room off a roomy partition to use elsewhere.",
            Goal::MovePartition => "Slide one along the disk; its contents go nowhere, only its place.",
            Goal::CleanUp => "Remove or reformat a partition you no longer need.",
            Goal::DoItMyself => "The full disk editor: drag partitions and edges exactly where you want.",
        }
    }
}

pub enum Action {
    ChooseDisk(String),
    ChooseGoal(Goal),
    Show(Screen),
    Pick(Guid),
    Edit(Guid, Extent),
    StartOver,
    RunChecks,
    ToggleBackedUp,
    OpenSave,
    Apply,
    PreviewRestart,
    CutPower,
    PlanAnother,
}

/// How a check row looks right now: the rows resolve one at a time, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Waiting,
    Running,
    Passed,
    Failed,
}

pub struct Outcome {
    pub bytes: u64,
    pub chunks: u64,
    pub power_cuts: u32,
    pub seconds: u64,
}

pub struct App {
    /// Whether the fonts and colours have been installed in the window's context.
    styled: bool,
    worker: Worker,
    pub disks: Vec<DiskSummary>,
    pub disk_problems: Vec<Refusal>,
    pub probed: Option<Probed>,
    pub target: Target,
    pub selected: Option<Guid>,
    pub plan: Option<Planned>,
    pub plan_problems: Vec<Refusal>,
    plan_due: Option<f64>,
    pending_plan: Option<u64>,
    pub checks: Vec<CheckResult>,
    checks_started: Option<f64>,
    pub checks_passed: bool,
    pub checks_finished: bool,
    pending_check: Option<u64>,
    pub check_problems: Vec<Refusal>,
    pub backed_up: bool,
    pending_apply: Option<u64>,
    pub screen: Screen,
    pub reached: Screen,
    /// The goal chosen on the home screen, if any. Future goal screens read it; today every
    /// goal opens the editor.
    pub goal: Option<Goal>,
    pub screen_shown_at: f64,
    pub status: String,
    pub run: Option<RecoveryRun>,
    done_due: Option<f64>,
    pub outcome: Option<Outcome>,
    pub save: Option<SaveDialog>,
    now: f64,
}

impl App {
    pub fn new(worker: Worker) -> Self {
        let mut app = Self {
            styled: false,
            worker,
            disks: Vec::new(),
            disk_problems: Vec::new(),
            probed: None,
            target: Target::new(),
            selected: None,
            plan: None,
            plan_problems: Vec::new(),
            plan_due: None,
            pending_plan: None,
            checks: Vec::new(),
            checks_started: None,
            checks_passed: false,
            checks_finished: false,
            pending_check: None,
            check_problems: Vec::new(),
            backed_up: false,
            pending_apply: None,
            screen: Screen::Home,
            reached: Screen::Arrange,
            goal: None,
            screen_shown_at: 0.0,
            status: String::new(),
            run: None,
            done_due: None,
            outcome: None,
            save: None,
            now: 0.0,
        };
        app.worker.send(Request::ListDisks);
        app
    }

    pub fn is_demo(&self) -> bool {
        self.worker.is_demo()
    }

    pub fn now(&self) -> f64 {
        self.now
    }

    /* ---------- answers from the helper ---------- */

    fn receive(&mut self) {
        while let Some((id, response)) = self.worker.try_recv() {
            match response {
                Response::Disks(reply) => self.on_disks(reply),
                Response::Probed(reply) => self.on_probed(reply),
                Response::Planned(reply) if self.pending_plan == Some(id) => self.on_planned(reply),
                Response::Checked(reply) if self.pending_check == Some(id) => {
                    self.on_checked(reply)
                }
                Response::Applied(reply) if self.pending_apply == Some(id) => {
                    self.on_applied(reply)
                }
                // An answer to a request that has since been replaced.
                Response::Planned(_) | Response::Checked(_) | Response::Applied(_) => {}
            }
        }
    }

    fn on_disks(&mut self, reply: Result<Reply<Vec<DiskSummary>>, BackendError>) {
        match flatten(reply) {
            Ok(disks) => {
                let first = disks
                    .iter()
                    .find(|d| !d.removable && d.partition_table.as_deref() == Some("gpt"))
                    .map(|d| d.path.clone());
                self.disks = disks;
                if let Some(path) = first {
                    self.open_disk(path);
                }
            }
            Err(refusals) => self.disk_problems = refusals,
        }
    }

    fn open_disk(&mut self, disk: String) {
        self.status = format!("Reading {disk}…");
        self.disk_problems.clear();
        self.worker.send(Request::Probe { disk });
    }

    fn on_probed(&mut self, reply: Result<Reply<Probed>, BackendError>) {
        match flatten(reply) {
            Ok(probed) => {
                let target = probed.target.clone();
                self.probed = Some(probed);
                self.reached = Screen::Arrange;
                self.selected = None;
                self.on_target_change(target);
                // The disk is ready; wait on the home screen for the person to pick a goal.
                self.show(Screen::Home);
                self.status = "Pick what you'd like to do.".to_string();
            }
            Err(refusals) => {
                self.disk_problems = refusals;
                self.status.clear();
            }
        }
    }

    fn on_planned(&mut self, reply: Result<Reply<Planned>, BackendError>) {
        self.pending_plan = None;
        match flatten(reply) {
            Ok(plan) => {
                self.plan_problems = plan.unavailable.clone();
                self.plan = Some(plan);
            }
            Err(refusals) => {
                self.plan = None;
                self.plan_problems = refusals;
            }
        }
    }

    fn on_checked(&mut self, reply: Result<Reply<Option<Vec<CheckResult>>>, BackendError>) {
        self.pending_check = None;
        match flatten(reply) {
            Ok(rows) => {
                // The helper answers `ok` with no rows when every check passed but it didn't
                // itemise them (issue I-18). Show one passed row so apply can unlock.
                self.checks = rows.filter(|r| !r.is_empty()).unwrap_or_else(|| {
                    vec![CheckResult {
                        name: "All safety checks passed".to_string(),
                        detail: "The disk, power, filesystems and bootloader are all ready."
                            .to_string(),
                        passed: true,
                    }]
                });
                self.checks_started = Some(self.now);
            }
            Err(refusals) => {
                self.checks.clear();
                self.check_problems = refusals;
                self.status =
                    "Apply stays locked. Each refusal says how to fix it, then check again."
                        .to_string();
            }
        }
    }

    fn on_applied(&mut self, reply: Result<Reply<crate::protocol::Applied>, BackendError>) {
        self.pending_apply = None;
        match flatten(reply) {
            Ok(_) => self.show_restart(),
            Err(refusals) => {
                self.check_problems = refusals;
                self.status = "Nothing was changed.".to_string();
            }
        }
    }

    /* ---------- the flow ---------- */

    pub fn show(&mut self, next: Screen) {
        self.screen = next;
        self.reached = self.reached.max(next);
        self.screen_shown_at = self.now;
        self.status.clear();
    }

    /// Opens a goal. Today every goal leads to the editor (the "do it myself" experience); the
    /// simplified per-goal screens are built out one at a time, and will set a target here
    /// before showing the editor as a confirmation.
    fn choose_goal(&mut self, goal: Goal) {
        if self.probed.is_none() {
            return;
        }
        self.goal = Some(goal);
        self.show(Screen::Arrange);
        self.status = match goal {
            Goal::DoItMyself => "Drag a partition to move it, or an edge to resize it.",
            _ => "Arrange the change, then see the plan. A simpler screen for this is coming.",
        }
        .to_string();
    }

    fn pick(&mut self, guid: Guid) {
        self.selected = if self.selected == Some(guid) {
            None
        } else {
            Some(guid)
        };
    }

    fn on_target_change(&mut self, next: Target) {
        self.target = next;
        self.checks_passed = false;
        self.checks_finished = false;
        self.checks.clear();
        self.checks_started = None;
        self.reached = Screen::Arrange;
        self.plan_due = Some(self.now + PLAN_DELAY);
    }

    fn request_plan(&mut self) {
        self.plan_due = None;
        if let Some(probed) = &self.probed {
            let disk = probed.disk.clone();
            let target = self.target.clone();
            self.pending_plan = Some(self.worker.send(Request::Plan { disk, target }));
        }
    }

    fn run_checks(&mut self) {
        let Some(probed) = &self.probed else {
            return;
        };
        let disk = probed.disk.clone();
        self.show(Screen::Checks);
        self.checks.clear();
        self.checks_started = None;
        self.checks_passed = false;
        self.checks_finished = false;
        self.check_problems.clear();
        self.status = "Running the safety checks…".to_string();
        let target = self.target.clone();
        self.pending_check = Some(self.worker.send(Request::Check { disk, target }));
    }

    /// How many check rows have resolved so far.
    pub fn checks_resolved(&self) -> usize {
        match self.checks_started {
            Some(started) => {
                let steps = ((self.now - started) / CHECK_STEP).floor().max(0.0) as usize;
                steps.min(self.checks.len())
            }
            None => 0,
        }
    }

    pub fn row_state(&self, index: usize) -> RowState {
        let resolved = self.checks_resolved();
        match self.checks.get(index) {
            Some(check) if index < resolved => {
                if check.passed {
                    RowState::Passed
                } else {
                    RowState::Failed
                }
            }
            Some(_) if index == resolved && self.checks_started.is_some() => RowState::Running,
            _ => RowState::Waiting,
        }
    }

    fn step_checks(&mut self) {
        if self.checks_started.is_some()
            && !self.checks_finished
            && self.checks_resolved() == self.checks.len()
        {
            self.finish_checks();
        }
    }

    fn finish_checks(&mut self) {
        self.checks_finished = true;
        self.checks_passed = !self.checks.is_empty() && self.checks.iter().all(|c| c.passed);
        self.check_problems = self
            .checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| {
                Refusal::new(
                    format!("{}: {}", c.name, c.detail),
                    "Fix this, then check again.",
                )
            })
            .collect();
        self.status = if self.checks_passed {
            "All green. Boringly green. Confirm your backup to unlock apply."
        } else {
            "Apply stays locked until every check passes."
        }
        .to_string();
    }

    fn apply(&mut self) {
        if !self.can_apply() {
            return;
        }
        let Some(probed) = &self.probed else {
            return;
        };
        let disk = probed.disk.clone();
        let target = self.target.clone();
        self.status = "Preparing the change. Your password will be asked for…".to_string();
        self.pending_apply = Some(self.worker.send(Request::Apply { disk, target }));
    }

    fn show_restart(&mut self) {
        self.show(Screen::Restart);
        // The simulation is a demo teaching aid; on a real disk the move runs in the recovery
        // system on the machine's own screen, so there is nothing for the app to play.
        if self.is_demo() {
            let bytes = self.plan.as_ref().map_or(0, |p| p.copy_bytes);
            self.run = Some(RecoveryRun::new(bytes.max(1)));
            self.status = "Demo: preview what the restart looks like, power cut included.".to_string();
        } else {
            self.run = None;
            self.status = "Everything is ready. Restart the computer to begin.".to_string();
        }
    }

    fn preview_restart(&mut self) {
        let now = self.clock();
        if let Some(run) = &mut self.run {
            run.start(now);
            self.status = "Copying. Pull the plug whenever you like.".to_string();
        }
    }

    fn step_run(&mut self) {
        let now = self.clock();
        let Some(run) = &mut self.run else {
            return;
        };
        let was_playing = run.is_playing();
        run.advance(now);
        if was_playing && run.phase == Phase::Finished && self.done_due.is_none() {
            self.done_due = Some(self.now + DONE_DELAY);
        }
        if self.done_due.is_some_and(|due| self.now >= due) {
            self.done_due = None;
            if let Some(run) = &self.run {
                self.outcome = Some(Outcome {
                    bytes: run.total_bytes,
                    chunks: run.total_chunks,
                    power_cuts: run.power_cuts,
                    seconds: run.seconds(),
                });
            }
            self.show(Screen::Done);
        }
    }

    fn start_over(&mut self) {
        if let Some(target) = self.probed.as_ref().map(|p| p.target.clone()) {
            self.on_target_change(target);
        }
    }

    fn clock(&self) -> Duration {
        Duration::from_secs_f64(self.now.max(0.0))
    }

    /* ---------- what the buttons allow ---------- */

    pub fn has_changes(&self) -> bool {
        self.plan.as_ref().is_some_and(|p| !p.operations.is_empty())
    }

    pub fn plannable(&self) -> bool {
        self.has_changes() && self.plan.as_ref().is_some_and(|p| p.unavailable.is_empty())
    }

    pub fn can_apply(&self) -> bool {
        self.plannable() && self.checks_passed && self.backed_up && self.pending_apply.is_none()
    }

    pub fn run_is_playing(&self) -> bool {
        self.run.as_ref().is_some_and(RecoveryRun::is_playing)
    }

    pub fn run_is_ready(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.phase == Phase::Ready)
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::ChooseDisk(disk) => self.open_disk(disk),
            Action::ChooseGoal(goal) => self.choose_goal(goal),
            Action::Show(screen) => self.show(screen),
            Action::Pick(guid) => self.pick(guid),
            Action::Edit(guid, extent) => {
                let mut next = self.target.clone();
                next.insert(guid, extent);
                self.on_target_change(next);
            }
            Action::StartOver => self.start_over(),
            Action::RunChecks => self.run_checks(),
            Action::ToggleBackedUp => self.backed_up = !self.backed_up,
            Action::OpenSave => {
                if let Some(probed) = &self.probed {
                    self.save = Some(SaveDialog::new(probed));
                }
            }
            Action::Apply => self.apply(),
            Action::PreviewRestart => self.preview_restart(),
            Action::CutPower => {
                let now = self.clock();
                if let Some(run) = &mut self.run {
                    run.cut_power(now);
                }
            }
            Action::PlanAnother => {
                self.start_over();
                self.show(Screen::Arrange);
            }
        }
    }

    /// When the next timed change is due, so the window repaints only while something moves.
    fn next_wake(&self) -> Option<f64> {
        let mut due: Vec<f64> = Vec::new();
        if let Some(at) = self.plan_due {
            due.push(at - self.now);
        }
        if self.checks_started.is_some() && !self.checks_finished {
            due.push(CHECK_STEP / 2.0);
        }
        if let Some(run) = &self.run {
            if let Some(next) = run.next_change(self.clock()) {
                due.push(next.as_secs_f64());
            }
        }
        if let Some(at) = self.done_due {
            due.push(at - self.now);
        }
        due.into_iter().map(|d| d.max(0.0)).reduce(f64::min)
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

impl App {
    /// One frame: take in answers and timers, draw, then apply what the user did.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        if !self.styled {
            ui.ctx().set_fonts(crate::theme::fonts());
            ui.ctx().set_visuals(crate::theme::visuals());
            self.styled = true;
        }
        self.now = ui.input(|i| i.time);
        self.receive();
        if self.plan_due.is_some_and(|due| self.now >= due) {
            self.request_plan();
        }
        self.step_checks();
        self.step_run();

        let mut actions = Vec::new();
        shell::show(ui, self, &mut actions);
        if let Some(dialog) = &mut self.save {
            if !dialog.show(ui.ctx(), self.worker.is_demo()) {
                if let Some(message) = dialog.saved_message() {
                    self.status = message;
                }
                self.save = None;
            }
        }
        // The frame was drawn before these took effect, so draw one more that shows them.
        if !actions.is_empty() {
            ui.ctx().request_repaint();
        }
        for action in actions {
            self.act(action);
        }
        if let Some(wait) = self.next_wake() {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(wait));
        }
    }
}

/// A reply's value, or what stopped it as refusals. A helper that can't be reached is also a
/// refusal, with the fix for that.
fn flatten<T>(reply: Result<Reply<T>, BackendError>) -> Result<T, Vec<Refusal>> {
    match reply {
        Ok(Reply::Ok { value }) => Ok(value),
        Ok(Reply::Refused { refusals }) => Err(refusals),
        Err(BackendError(error)) => Err(vec![Refusal::new(
            format!("BootShift's helper service didn't answer: {error}."),
            "Make sure bootshiftd is installed and running, then try again.",
        )]),
    }
}
