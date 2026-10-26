import { chooseBackend } from "./backend";
import { RecoveryConsole } from "./console";
import { DiskBar } from "./diskBar";
import { describeOperation, formatBytes } from "./format";
import { renderLegend } from "./legend";
import type { Planned, Probed, Refusal, Reply, Target } from "./types";
import "./styles.css";

const PLAN_DELAY_MS = 300;

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
  legend: element("legend"),
  reset: element<HTMLButtonElement>("reset"),
  toPlan: element<HTMLButtonElement>("to-plan"),
  steps: element("steps"),
  summary: element("summary"),
  problems: element("problems"),
  backToArrange: element<HTMLButtonElement>("back-to-arrange"),
  check: element<HTMLButtonElement>("check"),
  checkResults: element("check-results"),
  checksStatus: element("checks-status"),
  recheck: element<HTMLButtonElement>("recheck"),
  backedUp: element<HTMLInputElement>("backed-up"),
  apply: element<HTMLButtonElement>("apply"),
  console: element("console"),
  cutPower: element<HTMLButtonElement>("cut-power"),
  previewRestart: element<HTMLButtonElement>("preview-restart"),
  doneSub: element("done-sub"),
  doneStats: element("done-stats"),
  doneNote: element("done-note"),
  startAgain: element<HTMLButtonElement>("start-again"),
  status: element("status"),
};

let probed: Probed | undefined;
let target: Target = {};
let editor: DiskBar | undefined;
let plan: Planned | undefined;
let checksPassed = false;
let planTimer: number | undefined;
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
  renderStepper();
  setStatus("");
}

function renderStepper(): void {
  page.stepper.replaceChildren(
    ...SCREENS.map((name, index) => {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = `${index + 1} · ${SCREEN_NAMES[name]}`;
      button.className = "step";
      if (name === screen) button.classList.add("current");
      const reachable = SCREENS.indexOf(name) <= SCREENS.indexOf(reached);
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

/* ---------- arrange ---------- */

async function start(): Promise<void> {
  page.demoNotice.hidden = !backend.isDemo;
  renderStepper();
  const reply = await backend.listDisks();
  if (reply.result === "refused") return renderRefusals(page.problems, reply.refusals);
  page.diskSelect.replaceChildren(
    ...reply.value.map((disk) => new Option(`${disk.path} · ${formatBytes(disk.size)}`, disk.path)),
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
  show("arrange");
  new DiskBar(page.before, probed.layout, probed.target);
  editor = new DiskBar(page.after, probed.layout, probed.target, onTargetChange);
  onTargetChange(probed.target);
  setStatus("Drag partitions or their edges to arrange the disk.");
}

function onTargetChange(next: Target): void {
  target = next;
  checksPassed = false;
  reached = "arrange";
  renderStepper();
  if (probed) renderLegend(page.legend, probed.layout, next);
  window.clearTimeout(planTimer);
  planTimer = window.setTimeout(() => void refreshPlan(), PLAN_DELAY_MS);
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
  const hasChanges = (plan?.operations.length ?? 0) > 0;
  page.summary.hidden = !hasChanges;
  if (plan && hasChanges) {
    page.summary.replaceChildren();
    const bold = document.createElement("b");
    bold.textContent = `Copies about ${formatBytes(plan.copy_bytes)}, during one restart.`;
    page.summary.append(bold, " Keep working; the disk is untouched until then.");
  }
  renderRefusals(page.problems, reply.result === "refused" ? reply.refusals : (plan?.unavailable ?? []));
  updateButtons();
}

/* ---------- checks ---------- */

async function runChecks(): Promise<void> {
  if (!probed) return;
  show("checks");
  page.checkResults.replaceChildren();
  page.checksStatus.textContent = "Running the safety checks…";
  const reply = await backend.check(probed.disk, target);
  checksPassed = reply.result === "ok";
  if (checksPassed) {
    const row = listItem("Every check passed.");
    row.className = "pass";
    page.checkResults.replaceChildren(row);
    page.checksStatus.textContent = "All green. Boringly green. Apply unlocks once you confirm your backup.";
  } else {
    renderRefusals(page.checkResults, reply.result === "refused" ? reply.refusals : []);
    for (const row of page.checkResults.children) row.className = "fail";
    page.checksStatus.textContent =
      "Apply stays locked until every check passes. Each one says how to fix it; fix it, then check again.";
  }
  updateButtons();
}

/* ---------- restart ---------- */

async function apply(): Promise<void> {
  if (!probed || !plan) return;
  page.apply.disabled = true;
  setStatus("Preparing the change. Your password will be asked for…");
  const reply = await backend.apply(probed.disk, target);
  if (reply.result === "refused") {
    renderRefusals(page.checkResults, reply.refusals);
    for (const row of page.checkResults.children) row.className = "fail";
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
  if (backend.isDemo) {
    page.previewRestart.hidden = false;
    page.cutPower.hidden = true;
    setStatus("Demo: preview what the restart will look like, power cut included.");
  } else {
    page.previewRestart.hidden = true;
    page.cutPower.hidden = true;
    setStatus("Everything is ready. Restart the computer to begin.");
  }
}

function startSimulation(): void {
  page.previewRestart.hidden = true;
  page.cutPower.hidden = false;
  recovery?.start();
}

function onSimulationDone(outcome: { bytes: number; chunks: number; powerCuts: number; seconds: number }): void {
  page.cutPower.hidden = true;
  window.setTimeout(() => {
    show("done");
    page.doneSub.textContent = backend.isDemo
      ? "A simulated run, end to end. Every moved byte was read back, checked, and found exactly where it should be."
      : "The disk matches the plan. Every moved byte was read back and verified.";
    page.doneStats.replaceChildren(
      stat(formatBytes(outcome.bytes), "data moved"),
      stat(`${outcome.chunks}/${outcome.chunks}`, "chunks verified"),
      stat(`${outcome.powerCuts}`, outcome.powerCuts === 1 ? "power cut survived" : "power cuts survived"),
      stat(`${outcome.seconds}s`, backend.isDemo ? "simulated time" : "total time"),
    );
    page.doneNote.textContent =
      "The full log and your old partition table are kept on the EFI system partition. " +
      (backend.isDemo ? "In the real app, undo runs the same plan in reverse while the space is still free." : "");
  }, 1200);
}

/* ---------- shared ---------- */

function updateButtons(): void {
  const hasChanges = (plan?.operations.length ?? 0) > 0;
  const plannable = hasChanges && (plan?.unavailable.length ?? 1) === 0;
  page.reset.disabled = !hasChanges;
  page.toPlan.disabled = !hasChanges;
  page.check.disabled = !plannable;
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

page.reset.addEventListener("click", () => {
  if (!probed) return;
  editor?.update(probed.target);
  onTargetChange(probed.target);
});
page.toPlan.addEventListener("click", () => show("plan"));
page.backToArrange.addEventListener("click", () => show("arrange"));
page.check.addEventListener("click", () => void runChecks());
page.recheck.addEventListener("click", () => void runChecks());
page.backedUp.addEventListener("change", updateButtons);
page.apply.addEventListener("click", () => void apply());
page.previewRestart.addEventListener("click", startSimulation);
page.cutPower.addEventListener("click", () => void recovery?.cutPower());
page.startAgain.addEventListener("click", () => {
  if (!probed) return;
  editor?.update(probed.target);
  onTargetChange(probed.target);
  show("arrange");
});

void start();
