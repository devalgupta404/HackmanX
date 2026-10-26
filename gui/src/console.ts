// The recovery screen as the user will see it during the restart, drawn in the GUI so people
// know what to expect. In demo mode it can play a pretend run, including a power cut, so the
// resume story can be shown without touching a disk. The real recovery system prints the same
// lines on the machine's console.

import { formatBytes } from "./format";

const CHUNK_BYTES = 64 * 1024 ** 2;
const TICK_MS = 90;
/** The pretend run finishes in about 25 seconds, whatever the plan's size. */
const TICKS_TO_FINISH = 280;

export interface SimOutcome {
  bytes: number;
  chunks: number;
  powerCuts: number;
  seconds: number;
}

export class RecoveryConsole {
  private readonly root: HTMLElement;
  private readonly totalChunks: number;
  private chunk = 0;
  private powerCuts = 0;
  private startedAt = 0;
  private timer: number | undefined;
  private busy = false;

  constructor(
    container: HTMLElement,
    private readonly totalBytes: number,
    private readonly onFinished?: (outcome: SimOutcome) => void,
  ) {
    this.root = document.createElement("div");
    this.root.className = "console";
    this.root.setAttribute("role", "img");
    this.root.setAttribute("aria-label", "The BootShift recovery screen");
    container.replaceChildren(this.root);
    this.totalChunks = Math.max(1, Math.ceil(totalBytes / CHUNK_BYTES));
    this.render();
  }

  /** Plays the pretend run from the current chunk. */
  start(): void {
    if (this.timer !== undefined || this.busy) return;
    if (this.startedAt === 0) this.startedAt = Date.now();
    const perTick = Math.max(1, Math.round(this.totalChunks / TICKS_TO_FINISH));
    this.timer = window.setInterval(() => {
      this.chunk = Math.min(this.totalChunks, this.chunk + perTick);
      this.render();
      if (this.chunk >= this.totalChunks) this.finish();
    }, TICK_MS);
  }

  /** A pretend power cut: the screen goes dark, then recovery resumes from the journal. */
  async cutPower(): Promise<void> {
    if (this.busy || this.chunk >= this.totalChunks) return;
    this.busy = true;
    this.powerCuts += 1;
    this.stopTicking();
    const resumeFrom = this.chunk;
    this.root.replaceChildren();
    await pause(900);
    this.say("dim", "power restored. firmware hands over to BootShift recovery.");
    await pause(650);
    this.say("dim", `journal: plan found. last verified chunk ${resumeFrom} of ${this.totalChunks}.`);
    await pause(650);
    this.say("grn", `that was rude. resuming from chunk ${resumeFrom}.`);
    await pause(1100);
    this.busy = false;
    this.render();
    this.start();
  }

  stop(): void {
    this.stopTicking();
  }

  private finish(): void {
    this.stopTicking();
    this.render();
    this.onFinished?.({
      bytes: this.totalBytes,
      chunks: this.totalChunks,
      powerCuts: this.powerCuts,
      seconds: Math.round((Date.now() - this.startedAt) / 1000),
    });
  }

  private stopTicking(): void {
    window.clearInterval(this.timer);
    this.timer = undefined;
  }

  private render(): void {
    const share = this.chunk / this.totalChunks;
    const filled = Math.round(share * 36);
    const done = this.chunk >= this.totalChunks;
    this.root.replaceChildren();
    this.say("lit", "BootShift recovery");
    this.say("dim", "moving your partitions, before any operating system starts");
    this.rule();
    const bar = line("bar-line");
    bar.append(
      span("grn", "█".repeat(filled)),
      span("dim", "░".repeat(36 - filled)),
      span("lit", `  ${Math.floor(share * 100)}%`),
    );
    this.root.append(bar);
    this.say(
      "",
      `${formatBytes(this.totalBytes * share)} of ${formatBytes(this.totalBytes)} moved`,
    );
    this.say(
      "dim",
      done
        ? `all ${this.totalChunks} chunks copied, flushed, verified, journaled`
        : `chunk ${this.chunk} of ${this.totalChunks} copied, flushed, verified, journaled`,
    );
    this.rule();
    if (done) {
      this.say("grn", "Done. Restarting into your system.");
    } else {
      this.say("grn", "Power cut? Rude, but fine. Turn the computer back on.");
      this.say("grn", "BootShift resumes from the last verified chunk. No drama.");
      this.say("amb", "Do not boot Windows from the firmware menu. A half-moved disk");
      this.say("amb", "is not something it handles gracefully.");
    }
  }

  private say(kind: string, text: string): void {
    const element = line(kind);
    element.textContent = text;
    this.root.append(element);
  }

  private rule(): void {
    this.root.append(document.createElement("hr"));
  }
}

function line(className: string): HTMLDivElement {
  const element = document.createElement("div");
  if (className) element.className = className;
  return element;
}

function span(className: string, text: string): HTMLSpanElement {
  const element = document.createElement("span");
  element.className = className;
  element.textContent = text;
  return element;
}

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}
