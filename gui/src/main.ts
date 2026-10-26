import { chooseBackend } from "./backend";
import { RecoveryConsole } from "./console";
import { renderDelta } from "./delta";
import { DiskBar } from "./diskBar";
import { describeOperation, formatBytes } from "./format";
import { renderTable } from "./legend";
import { renderReadout } from "./readout";
import type { CheckResult, Guid, Planned, Probed, Refusal, Reply, Target } from "./types";
import "./styles.css";

const PLAN_DELAY_MS = 300;
const CHECK_STEP_MS = 110;

const SCREENS = ["arrange", "plan", "checks", "restart", "done"] as const;
type Screen = (typeof SCREENS)[number];
const SCREEN_NAMES: Record<Screen, string> = {
  arrange: "Arrange",
  plan: "Plan",
  checks: "Checks",
  restart: "Restart",
  done: "Done",
};

const backend = chooseBackend();
const page = {
  stepper: element("stepper"),
  diskSelect: element<HTMLSelectElement>("disk"),
  demoNotice: element("demo-notice"),
  before: element("before"),
  after: element("after"),
  delta: element("delta"),
  legend: element("legend"),
  readout: element("readout"),
  reset: element<HTMLButtonElement>("reset"),
  toPlan: element<HTMLButtonElement>("to-plan"),
  steps: element("steps"),
  problems: element("problems"),
  backToArrange: element<HTMLButtonElement>("back-to-arrange"),
  check: element<HTMLButtonElement>("check"),
  checkResults: element("check-results"),
  checkProblems: element("check-problems"),
  backedUp: element<HTMLInputElement>("backed-up"),
  saveBackup: element<HTMLButtonElement>("save-backup"),
  recheck: element<HTMLButtonElement>("recheck"),
  apply: element<HTMLButtonElement>("apply"),
  console: element("console"),
  cutPower: element<HTMLButtonElement>("cut-power"),
  previewRestart: element<HTMLButtonElement>("preview-restart"),
  doneSub: element("done-sub"),
  doneStats: element("done-stats"),
  doneDisk: element("done-disk"),
  doneNote: element("done-note"),
  startAgain: element<HTMLButtonElement>("start-again"),
  status: element("status"),
};

let probed: Probed | undefined;
let target: Target = {};
let editor: DiskBar | undefined;
let reference: DiskBar | undefined;
let plan: Planned | undefined;
let selected: Guid | undefined;
let checks: CheckResult[] = [];
let checksPassed = false;
let planTimer: number | undefined;
let checkTimers: number[] = [];
let screen: Screen = "arrange";
let reached: Screen = "arrange";
let recovery: RecoveryConsole | undefined;

function element<T extends HTMLElement = HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`the page has no #${id}`);
  return found as T;
}

/* ---------- screens ---------- */

function show(next: Screen): void {
  screen = next;
  if (SCREENS.indexOf(next) > SCREENS.indexOf(reached)) reached = next;
  for (const name of SCREENS) {
    element(`screen-${name}`).hidden = name !== next;
  }
  for (const acts of document.querySelectorAll<HTMLElement>(".acts")) {
    acts.hidden = acts.dataset.for !== next;
  }
  renderStepper();
  refreshReadout();
  setStatus("");
}

function renderStepper(): void {
  page.stepper.replaceChildren(
    ...SCREENS.map((name, index) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "step";
      const number = document.createElement("span");
      number.className = "step-n";
      number.textContent = String(index + 1);
      const label = document.createElement("span");
      label.className = "step-name";
      label.textContent = SCREEN_NAMES[name];
      button.append(number, label);
      const position = SCREENS.indexOf(name);
      if (name === screen) button.classList.add("current");
      if (position < SCREENS.indexOf(screen)) button.classList.add("done");
      const reachable = position <= SCREENS.indexOf(reached);
      if (reachable && name !== screen) {
        button.classList.add("reachable");
        button.addEventListener("click", () => show(name));
      } else {
        button.disabled = name !== screen;
      }
      return button;
    }),
  );
}

function refreshReadout(): void {
  renderReadout(page.readout, {
    screen,
    probed,
    target,
    plan,
    selected,
    checksPassed,
    checksTotal: checks.length,
    checksDone: checks.filter((check) => check.passed).length,
    isDemo: backend.isDemo,
  });
}

/* ---------- arrange ---------- */

async function start(): Promise<void> {
  page.demoNotice.hidden = !backend.isDemo;
  renderStepper();
  const reply = await backend.listDisks();
  if (reply.result === "refused") return renderRefusals(page.problems, reply.refusals);
  page.diskSelect.replaceChildren(
    ...reply.value.map((disk) => new Option(`${disk.path}  ${formatBytes(disk.size)}`, disk.path)),
  );
  page.diskSelect.addEventListener("change", () => void openDisk(page.diskSelect.value));
  const first = reply.value.find((disk) => !disk.removable && disk.partition_table === "gpt");
  if (first) {
    page.diskSelect.value = first.path;
    await openDisk(first.path);
  }
}

async function openDisk(disk: string): Promise<void> {
  setStatus(`Reading ${disk}…`);
  const reply = await backend.probe(disk);
  if (reply.result === "refused") return renderRefusals(page.problems, reply.refusals);
  probed = reply.value;
  reached = "arrange";
  selected = undefined;
  show("arrange");
  reference = new DiskBar(page.before, probed.layout, probed.target, undefined, pick);
  editor = new DiskBar(page.after, probed.layout, probed.target, onTargetChange, pick);
  onTargetChange(probed.target);
  setStatus("Drag a partition to move it, or an edge to resize it.");
}

function pick(guid: Guid): void {
  selected = selected === guid ? undefined : guid;
  editor?.select(selected);
  reference?.select(selected);
  if (probed) renderTable(page.legend, probed.layout, target, selected, pick);
  refreshReadout();
}

function onTargetChange(next: Target): void {
  target = next;
  checksPassed = false;
  checks = [];
  reached = "arrange";
  renderStepper();
  if (probed) {
    renderTable(page.legend, probed.layout, next, selected, pick);
    renderDelta(page.delta, probed.layout, next);
  }
  window.clearTimeout(planTimer);
  planTimer = window.setTimeout(() => void refreshPlan(), PLAN_DELAY_MS);
  refreshReadout();
  updateButtons();
}

/* ---------- plan ---------- */

async function refreshPlan(): Promise<void> {
  if (!probed) return;
  const reply: Reply<Planned> = await backend.plan(probed.disk, target);
  plan = reply.result === "ok" ? reply.value : undefined;
  const layout = probed.layout;
  page.steps.replaceChildren(
    ...(plan?.operations ?? []).map((operation) => listItem(describeOperation(layout, operation))),
  );
  renderRefusals(page.problems, reply.result === "refused" ? reply.refusals : (plan?.unavailable ?? []));
  refreshReadout();
  updateButtons();
}

/* ---------- checks ---------- */

async function runChecks(): Promise<void> {
  if (!probed) return;
  show("checks");
  for (const timer of checkTimers) window.clearTimeout(timer);
  checkTimers = [];
  checks = [];
  checksPassed = false;
  page.checkProblems.replaceChildren();
  setStatus("Running the safety checks…");
  updateButtons();

  const reply = await backend.check(probed.disk, target);
  if (reply.result === "refused") {
    checks = [];
    page.checkResults.replaceChildren();
    renderRefusals(page.checkProblems, reply.refusals);
    setStatus("Apply stays locked. Each refusal says how to fix it, then check again.");
    refreshReadout();
    return updateButtons();
  }

  checks = reply.value ?? [];
  renderChecks(0);
  // Resolving one row at a time shows the checks running rather than appearing all at once.
  checks.forEach((_, index) => {
    checkTimers.push(
      window.setTimeout(() => {
        renderChecks(index + 1);
        if (index === checks.length - 1) finishChecks();
      }, CHECK_STEP_MS * (index + 1)),
    );
  });
  if (checks.length === 0) finishChecks();
}

function renderChecks(resolved: number): void {
  page.checkResults.replaceChildren(
    ...checks.map((check, index) => {
      const row = document.createElement("li");
      row.className = index < resolved ? (check.passed ? "pass" : "fail") : index === resolved ? "busy" : "";
      const mark = document.createElement("span");
      mark.className = "mark";
      const what = document.createElement("span");
      what.className = "what";
      what.textContent = check.name;
      const detail = document.createElement("span");
      detail.className = "detail";
      detail.textContent = index < resolved ? check.detail : "";
      row.append(mark, what, detail);
      return row;
    }),
  );
}

function finishChecks(): void {
  checksPassed = checks.length > 0 && checks.every((check) => check.passed);
  const failed = checks.filter((check) => !check.passed);
  renderRefusals(
    page.checkProblems,
    failed.map((check) => ({ reason: `${check.name}: ${check.detail}`, fix: "Fix this, then check again." })),
  );
  setStatus(
    checksPassed
      ? "All green. Boringly green. Confirm your backup to unlock apply."
      : "Apply stays locked until every check passes.",
  );
  refreshReadout();
  updateButtons();
}

function saveBackup(): void {
  if (!probed) return;
  const text = backupText(probed);
  const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = `bootshift-partition-table-${probed.disk.split("/").pop()}.json`;
  link.click();
  URL.revokeObjectURL(url);
  setStatus("Partition table saved. Keep it somewhere that is not this disk.");
}

function backupText(current: Probed): string {
  return JSON.stringify(
    {
      note: backend.isDemo
        ? "Demo data. The real backup is the raw GPT, written to the EFI partition before anything moves."
        : "A readable copy of the partition table as it is now. The raw GPT also goes on the EFI partition.",
      disk: current.disk,
      size: current.size,
      geometry: current.layout.geometry,
      partitions: current.layout.partitions,
    },
    null,
    2,
  );
}

/* ---------- restart ---------- */

async function apply(): Promise<void> {
  if (!probed || !plan) return;
  page.apply.disabled = true;
  setStatus("Preparing the change. Your password will be asked for…");
  const reply = await backend.apply(probed.disk, target);
  if (reply.result === "refused") {
    renderRefusals(page.checkProblems, reply.refusals);
    setStatus("Nothing was changed.");
    return updateButtons();
  }
  showRestartScreen();
}

function showRestartScreen(): void {
  if (!plan) return;
  show("restart");
  recovery?.stop();
  recovery = new RecoveryConsole(page.console, Math.max(plan.copy_bytes, 1), onSimulationDone);
  page.cutPower.hidden = true;
  page.previewRestart.hidden = !backend.isDemo;
  setStatus(
    backend.isDemo
      ? "Demo: preview what the restart looks like, power cut included."
      : "Everything is ready. Restart the computer to begin.",
  );
}

function startSimulation(): void {
  page.previewRestart.hidden = true;
  page.cutPower.hidden = false;
  setStatus("Copying. Pull the plug whenever you like.");
  recovery?.start();
}

function onSimulationDone(outcome: { bytes: number; chunks: number; powerCuts: number; seconds: number }): void {
  page.cutPower.hidden = true;
  window.setTimeout(() => {
    show("done");
    page.doneSub.textContent = backend.isDemo
      ? "Every moved byte was read back and verified. The power cut in the middle changed nothing."
      : "The disk matches the plan. Every moved byte was read back and verified.";
    if (probed) new DiskBar(page.doneDisk, probed.layout, target);
    page.doneStats.replaceChildren(
      stat(formatBytes(outcome.bytes), "data moved"),
      stat(`${outcome.chunks}`, "chunks verified"),
      stat(`${outcome.powerCuts}`, outcome.powerCuts === 1 ? "power cut survived" : "power cuts survived"),
      stat(`${outcome.seconds}s`, backend.isDemo ? "simulated time" : "total time"),
    );
    page.doneNote.textContent =
      "The full log and your old partition table stay on the EFI system partition until the next change.";
    refreshReadout();
  }, 1200);
}

/* ---------- shared ---------- */

function updateButtons(): void {
  const hasChanges = (plan?.operations.length ?? 0) > 0;
  const plannable = hasChanges && (plan?.unavailable.length ?? 1) === 0;
  page.reset.disabled = !hasChanges;
  page.toPlan.disabled = !hasChanges;
  page.check.disabled = !plannable;
  page.saveBackup.disabled = !probed;
  page.apply.disabled = !(plannable && checksPassed && page.backedUp.checked);
}

function renderRefusals(container: HTMLElement, refusals: Refusal[]): void {
  container.replaceChildren(
    ...refusals.map((refusal) => {
      const item = listItem(refusal.reason);
      const fix = document.createElement("p");
      fix.className = "fix";
      fix.textContent = refusal.fix;
      item.append(fix);
      return item;
    }),
  );
}

function listItem(text: string): HTMLLIElement {
  const item = document.createElement("li");
  const reason = document.createElement("span");
  reason.textContent = text;
  item.append(reason);
  return item;
}

function stat(value: string, key: string): HTMLDivElement {
  const box = document.createElement("div");
  box.className = "stat";
  const val = document.createElement("div");
  val.className = "s-val";
  val.textContent = value;
  const name = document.createElement("div");
  name.className = "s-key";
  name.textContent = key;
  box.append(val, name);
  return box;
}

function setStatus(text: string): void {
  page.status.textContent = text;
}

function startOver(): void {
  if (!probed) return;
  editor?.update(probed.target);
  onTargetChange(probed.target);
}

page.reset.addEventListener("click", startOver);
page.toPlan.addEventListener("click", () => show("plan"));
page.backToArrange.addEventListener("click", () => show("arrange"));
page.check.addEventListener("click", () => void runChecks());
page.recheck.addEventListener("click", () => void runChecks());
page.backedUp.addEventListener("change", updateButtons);
page.saveBackup.addEventListener("click", saveBackup);
page.apply.addEventListener("click", () => void apply());
page.previewRestart.addEventListener("click", startSimulation);
page.cutPower.addEventListener("click", () => void recovery?.cutPower());
page.startAgain.addEventListener("click", () => {
  startOver();
  show("arrange");
});

void start();
