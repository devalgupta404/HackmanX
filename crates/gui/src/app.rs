//! The app's state and the flow between screens. Screens draw from this state and hand back
//! `Action`s; the app applies them after drawing, so nothing changes halfway through a frame.

use std::time::Duration;

use bootshift_core::layout::{Extent, Guid, Layout, Target};
use bootshift_core::refusal::Refusal;
use bootshift_core::units::Sectors;
use eframe::egui;

use crate::backend::{BackendError, Request, Response, Worker};
use crate::editing;
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
    /// The migration goal's own screen, outside the partition flow.
    Migrate,
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
            Screen::Migrate => "Migrate",
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
    Migrate,
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
            Goal::Migrate => "Migrate to new hardware",
            Goal::DoItMyself => "Do it myself",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Goal::InstallOs => {
                "Make room for Linux or another OS. BootShift frees a clean block for the installer."
            }
            Goal::GiveSpace => {
                "Pick the one that's full. BootShift finds nearby space and grows it."
            }
            Goal::FreeSpace => "Carve some room off a roomy partition to use elsewhere.",
            Goal::MovePartition => {
                "Slide one along the disk; its contents go nowhere, only its place."
            }
            Goal::CleanUp => "Remove or reformat a partition you no longer need.",
            Goal::Migrate => {
                "Replicate this whole machine onto new hardware while it runs, then cut over with a \
                 signed, verified copy."
            }
            Goal::DoItMyself => {
                "The full disk editor: drag partitions and edges exactly where you want."
            }
        }
    }
}

/// A sensible partition to start a goal on: the largest one the goal can act on.
fn default_goal_partition(layout: &Layout, goal: Goal) -> Option<Guid> {
    let largest = |capable: fn(&bootshift_core::layout::Partition) -> bool| {
        layout
            .partitions
            .iter()
            .filter(|p| capable(p))
            .max_by_key(|p| p.extent.len.0)
            .map(|p| p.unique_guid)
    };
    match goal {
        Goal::GiveSpace => largest(|p| p.content.can_grow()),
        Goal::FreeSpace | Goal::InstallOs => largest(|p| p.content.can_shrink()),
        Goal::MovePartition => largest(|p| p.content.can_move()),
        Goal::CleanUp | Goal::Migrate | Goal::DoItMyself => None,
    }
}

pub enum Action {
    ChooseDisk(String),
    ChooseGoal(Goal),
    /// On a goal screen: act on this partition.
    GoalPick(Guid),
    /// On a grow/shrink/free goal: change by this many sectors.
    GoalAmount(Sectors),
    /// On the move goal: toward the start of the disk (true) or the end (false).
    GoalMove(bool),
    Show(Screen),
    Pick(Guid),
    Edit(Guid, Extent),
    /// Remove a partition from the layout, freeing its space.
    Delete(Guid),
    /// Put a deleted partition back at its original place.
    Restore(Guid),
    StartOver,
    RunChecks,
    ToggleBackedUp,
    OpenSave,
    Apply,
    PreviewRestart,
    CutPower,
    PlanAnother,
    /// On the migration screen: freeze and commit the cutover.
    MigrateCutover,
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
    /// The goal chosen on the home screen, if any.
    pub goal: Option<Goal>,
    /// The partition a simplified goal screen acts on.
    pub goal_partition: Option<Guid>,
    /// How much a grow/shrink/free goal changes the partition by.
    pub goal_amount: Sectors,
    /// For the move goal: toward the start of the disk when true, the end when false.
    pub goal_move_left: bool,
    pub screen_shown_at: f64,
    pub status: String,
    pub run: Option<RecoveryRun>,
    done_due: Option<f64>,
    pub outcome: Option<Outcome>,
    pub save: Option<SaveDialog>,
    /// The migration goal's own view and demo migration.
    pub migrate: crate::migrate::MigrateView,
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
            goal_partition: None,
            goal_amount: Sectors(0),
            goal_move_left: true,
            screen_shown_at: 0.0,
            status: String::new(),
            run: None,
            done_due: None,
            outcome: None,
            save: None,
            migrate: crate::migrate::MigrateView::new(),
            now: 0.0,
        };
        app.worker.send(Request::ListDisks);
        // When the host demo runner launches the app to drive a real migration, open straight on
        // the migration screen instead of the goal launcher.
        if std::env::var_os("BOOTSHIFT_MIGRATE_DEMO").is_some() {
            app.screen = Screen::Migrate;
            app.status = "Replicating this machine onto the new one.".to_string();
        }
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

    /// Opens a goal. `DoItMyself` is the editor; the others are simplified screens that build a
    /// target from one partition and an amount, and leave `Clean up` as advice for now.
    fn choose_goal(&mut self, goal: Goal) {
        if goal == Goal::Migrate {
            self.migrate.reset();
            self.show(Screen::Migrate);
            self.status = "Replicating this machine onto the new one.".to_string();
            return;
        }
        let Some(probed) = self.probed.clone() else {
            return;
        };
        self.goal = Some(goal);
        self.goal_move_left = true;
        self.goal_partition = default_goal_partition(&probed.layout, goal);
        self.goal_amount = Sectors(0);
        self.target = probed.target.clone();
        self.selected = self.goal_partition;
        if goal != Goal::DoItMyself {
            self.recompute_goal_target();
        }
        self.show(Screen::Arrange);
        self.status = match goal {
            Goal::DoItMyself => "Drag a partition to move it, or an edge to resize it.",
            Goal::CleanUp => "Choose what you no longer need.",
            _ => "Pick the partition and how much, then see the plan.",
        }
        .to_string();
    }

    /// Rebuilds the target from scratch for the current goal: the disk as it is, plus the one
    /// change the goal makes. The engine's checks still have the final say on the plan screen.
    fn recompute_goal_target(&mut self) {
        let Some(probed) = self.probed.clone() else {
            return;
        };
        let layout = &probed.layout;
        let mut target = probed.target.clone();
        if let (Some(goal), Some(guid)) = (self.goal, self.goal_partition) {
            if goal == Goal::GiveSpace {
                // Growing may take space from a neighbour, so it changes more than one partition.
                let current_len = layout.partition(guid).map_or(0, |p| p.extent.len.0);
                let goal_len = current_len + self.goal_amount.0;
                target = editing::grow_with_neighbours(layout, &target, guid, goal_len);
            } else if let Some(extent) = self.goal_extent(layout, &target, goal, guid) {
                target.insert(guid, extent);
            }
        }
        self.on_target_change(target);
    }

    /// The new extent of the goal's partition, or `None` to leave it unchanged.
    fn goal_extent(
        &self,
        layout: &Layout,
        target: &Target,
        goal: Goal,
        guid: Guid,
    ) -> Option<Extent> {
        let current = *target.get(&guid)?;
        let amount = self.goal_amount.0 as f64;
        match goal {
            // GiveSpace is built by `grow_with_neighbours` in `recompute_goal_target`, because it
            // can change a neighbour too, not just this partition.
            Goal::GiveSpace => None,
            Goal::FreeSpace | Goal::InstallOs => {
                editing::set_end(layout, target, guid, current.end().0 as f64 - amount)
            }
            Goal::MovePartition => {
                let (left, right) = editing::neighbour_bounds(layout, target, guid);
                let dest = if self.goal_move_left {
                    left as f64
                } else {
                    right.saturating_sub(current.len.0) as f64
                };
                editing::move_to(layout, target, guid, dest)
            }
            Goal::CleanUp | Goal::Migrate | Goal::DoItMyself => None,
        }
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
            self.status =
                "Demo: preview what the restart looks like, power cut included.".to_string();
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

    /// Removes a partition from the target, so the plan deletes it and frees its space.
    fn delete_partition(&mut self, guid: Guid) {
        let mut next = self.target.clone();
        if next.remove(&guid).is_some() {
            self.selected = None;
            self.on_target_change(next);
        }
    }

    /// Puts a deleted partition back where it started.
    fn restore_partition(&mut self, guid: Guid) {
        let Some(extent) = self
            .probed
            .as_ref()
            .and_then(|p| p.layout.partition(guid))
            .map(|p| p.extent)
        else {
            return;
        };
        let mut next = self.target.clone();
        next.insert(guid, extent);
        self.on_target_change(next);
    }

    /// Whether `guid` is being deleted: it is on the disk but left out of the target.
    pub fn is_deleted(&self, guid: Guid) -> bool {
        self.probed
            .as_ref()
            .is_some_and(|p| p.layout.partition(guid).is_some())
            && !self.target.contains_key(&guid)
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
            Action::GoalPick(guid) => {
                self.goal_partition = Some(guid);
                self.recompute_goal_target();
            }
            Action::GoalAmount(amount) => {
                self.goal_amount = amount;
                self.recompute_goal_target();
            }
            Action::GoalMove(left) => {
                self.goal_move_left = left;
                self.recompute_goal_target();
            }
            Action::Show(screen) => self.show(screen),
            Action::Pick(guid) => self.pick(guid),
            Action::Edit(guid, extent) => {
                let mut next = self.target.clone();
                next.insert(guid, extent);
                self.on_target_change(next);
            }
            Action::Delete(guid) => self.delete_partition(guid),
            Action::Restore(guid) => self.restore_partition(guid),
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
            Action::MigrateCutover => self.migrate.cutover(),
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
        if self.screen == Screen::Migrate {
            self.migrate.tick(self.now);
            self.migrate.poll();
        }

        let mut actions = Vec::new();
        shell::show(ui, self, &mut actions);
        if self.screen == Screen::Migrate && self.migrate.animating() {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
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
