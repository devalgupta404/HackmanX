import { chooseBackend } from "./backend";
import { DiskBar } from "./diskBar";
import { describeOperation, formatBytes } from "./format";
import type { Planned, Probed, Refusal, Reply, Target } from "./types";
import "./styles.css";

const PLAN_DELAY_MS = 300;

const backend = chooseBackend();
const page = {
  diskSelect: element<HTMLSelectElement>("disk"),
  demoNotice: element("demo-notice"),
  before: element("before"),
  after: element("after"),
  steps: element("steps"),
  summary: element("summary"),
  problems: element("problems"),
  reset: element<HTMLButtonElement>("reset"),
  check: element<HTMLButtonElement>("check"),
  backedUp: element<HTMLInputElement>("backed-up"),
  apply: element<HTMLButtonElement>("apply"),
  status: element("status"),
};

let probed: Probed | undefined;
let target: Target = {};
let editor: DiskBar | undefined;
let plan: Planned | undefined;
let checksPassed = false;
let planTimer: number | undefined;

function element<T extends HTMLElement = HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`the page has no #${id}`);
  return found as T;
}

async function start(): Promise<void> {
  page.demoNotice.hidden = !backend.isDemo;
  const reply = await backend.listDisks();
  if (reply.result === "refused") return showProblems(reply.refusals);
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
  if (reply.result === "refused") return showProblems(reply.refusals);
  probed = reply.value;
  new DiskBar(page.before, probed.layout, probed.target);
  editor = new DiskBar(page.after, probed.layout, probed.target, onTargetChange);
  onTargetChange(probed.target);
  setStatus("Drag partitions or their edges to arrange the disk.");
}

function onTargetChange(next: Target): void {
  target = next;
  checksPassed = false;
  showProblems([]);
  window.clearTimeout(planTimer);
  planTimer = window.setTimeout(() => void refreshPlan(), PLAN_DELAY_MS);
  updateButtons();
}

async function refreshPlan(): Promise<void> {
  if (!probed) return;
  const reply: Reply<Planned> = await backend.plan(probed.disk, target);
  plan = reply.result === "ok" ? reply.value : undefined;
  const layout = probed.layout;
  page.steps.replaceChildren(
    ...(plan?.operations ?? []).map((operation) => listItem(describeOperation(layout, operation))),
  );
  page.summary.textContent = !plan
    ? ""
    : plan.operations.length === 0
      ? "No changes yet."
      : `Copies about ${formatBytes(plan.copy_bytes)}, during one restart.`;
  showProblems(reply.result === "refused" ? reply.refusals : (plan?.unavailable ?? []));
  updateButtons();
}

async function runChecks(): Promise<void> {
  if (!probed) return;
  setStatus("Running the safety checks…");
  const reply = await backend.check(probed.disk, target);
  checksPassed = reply.result === "ok";
  showProblems(reply.result === "refused" ? reply.refusals : []);
  setStatus(checksPassed ? "Every check passed." : "Some checks failed. Each one says how to fix it.");
  updateButtons();
}

async function apply(): Promise<void> {
  if (!probed) return;
  page.apply.disabled = true;
  setStatus("Preparing the change. Your password will be asked for…");
  const reply = await backend.apply(probed.disk, target);
  if (reply.result === "refused") {
    showProblems(reply.refusals);
    setStatus("Nothing was changed.");
    return updateButtons();
  }
  setStatus("Everything is ready. Restart the computer to begin. If the power goes out during the change, just turn it back on.");
}

function updateButtons(): void {
  const hasChanges = (plan?.operations.length ?? 0) > 0;
  const plannable = hasChanges && (plan?.unavailable.length ?? 1) === 0;
  page.reset.disabled = !hasChanges;
  page.check.disabled = !plannable;
  page.apply.disabled = !(plannable && checksPassed && page.backedUp.checked);
}

function showProblems(refusals: Refusal[]): void {
  page.problems.replaceChildren(
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
  item.textContent = text;
  return item;
}

function setStatus(text: string): void {
  page.status.textContent = text;
}

page.reset.addEventListener("click", () => {
  if (!probed) return;
  editor?.update(probed.target);
  onTargetChange(probed.target);
});
page.check.addEventListener("click", () => void runChecks());
page.backedUp.addEventListener("change", updateButtons);
page.apply.addEventListener("click", () => void apply());

void start();
