// The right-hand panel. It always answers the question the current screen raises: what is this
// partition, what will this plan cost, what is protecting me, what just happened.

import { minLen } from "./editing";
import { contentName, formatBytes, sectorsToBytes } from "./format";
import type { Guid, Planned, Probed, Target } from "./types";

/** Sequential read-write speed used only to turn bytes into a rough time. */
const BYTES_PER_SECOND = 450 * 1024 ** 2;

export interface Context {
  screen: string;
  probed?: Probed;
  target: Target;
  plan?: Planned;
  selected?: Guid;
  checksPassed: boolean;
  checksTotal: number;
  checksDone: number;
  isDemo: boolean;
}

export function renderReadout(container: HTMLElement, context: Context): void {
  const blocks =
    context.screen === "arrange"
      ? arrangeBlocks(context)
      : context.screen === "plan"
        ? planBlocks(context)
        : context.screen === "checks"
          ? checkBlocks(context)
          : context.screen === "restart"
            ? restartBlocks()
            : doneBlocks(context);
  container.replaceChildren(...blocks);
}

function arrangeBlocks(context: Context): HTMLElement[] {
  const probed = context.probed;
  if (!probed) return [heading("Disk"), text("Reading the disk…")];
  const layout = probed.layout;
  const size = (sectors: number) => formatBytes(sectorsToBytes(layout, sectors));
  const partition = layout.partitions.find((p) => p.unique_guid === context.selected);

  if (!partition) {
    const taken = Object.values(context.target).reduce((sum, extent) => sum + extent.len, 0);
    return [
      heading("This disk"),
      facts([
        ["Device", probed.disk],
        ["Capacity", formatBytes(probed.size)],
        ["Partition table", "GPT"],
        ["Partitions", String(layout.partitions.length)],
        ["Unallocated", size(Math.max(0, layout.geometry.usable.len - taken))],
        ["Alignment", `${formatBytes(sectorsToBytes(layout, layout.geometry.alignment))} boundaries`],
      ]),
      block(heading("Pick a partition"), text("Click a partition in the bar or the table to see what it holds and how far it can go.")),
    ];
  }

  const extent = context.target[partition.unique_guid] ?? partition.extent;
  const usedShare = Math.min(100, (partition.used / extent.len) * 100);
  const smallest = minLen(layout, partition);
  const title = document.createElement("div");
  title.className = "rtitle";
  const swatch = document.createElement("span");
  swatch.className = `sw content-${partition.content.toLowerCase()}`;
  const name = document.createElement("span");
  name.textContent = partition.name || contentName(partition.content);
  title.append(swatch, name);

  const meter = document.createElement("div");
  meter.className = "meter";
  const fill = document.createElement("span");
  fill.style.width = `${usedShare}%`;
  meter.append(fill);

  return [
    block(
      title,
      facts([
        ["Contents", contentName(partition.content)],
        ["Partition number", String(partition.number)],
      ]),
    ),
    block(
      heading("Space"),
      meter,
      facts([
        ["Used", formatBytes(sectorsToBytes(layout, partition.used))],
        ["Size now", size(partition.extent.len)],
        ["Size after", size(extent.len), extent.len !== partition.extent.len ? "up" : ""],
        ["Smallest it can be", size(smallest)],
      ]),
    ),
    block(
      heading("Position"),
      facts([
        ["Starts now", `sector ${partition.extent.start.toLocaleString()}`],
        ["Starts after", `sector ${extent.start.toLocaleString()}`, extent.start !== partition.extent.start ? "up" : ""],
        [
          "Data to copy",
          extent.start === partition.extent.start
            ? "none"
            : formatBytes(sectorsToBytes(layout, Math.min(partition.used, extent.len))),
          extent.start !== partition.extent.start ? "up" : "",
        ],
      ]),
    ),
  ];
}

function planBlocks(context: Context): HTMLElement[] {
  const bytes = context.plan?.copy_bytes ?? 0;
  if (bytes === 0 && (context.plan?.operations.length ?? 0) === 0) {
    return [heading("Nothing to do"), text("The layout matches the disk. Move something on the arrange screen and the plan fills in.")];
  }
  const seconds = Math.max(1, Math.round(bytes / BYTES_PER_SECOND));
  return [
    heading("What this costs"),
    facts([
      ["Steps", String(context.plan?.operations.length ?? 0)],
      ["Data copied", formatBytes(bytes)],
      ["Chunk size", "64 MiB"],
      ["Restarts", "1"],
      ["Rough time", `${Math.floor(seconds / 60)}m ${seconds % 60}s`],
    ]),
    block(
      heading("What protects you"),
      text("Data is copied and verified in its new place before the partition table is touched."),
      text("A chunk is recorded only after the drive confirms the write, and only one write is in flight at a time."),
      text("The journal on the EFI partition is the record of truth, so a power cut resumes from the last verified chunk."),
    ),
  ];
}

function checkBlocks(context: Context): HTMLElement[] {
  const tally = context.checksTotal === 0 ? "—" : `${context.checksDone} of ${context.checksTotal}`;
  return [
    heading("Checks"),
    facts([
      ["Passed", tally, context.checksPassed ? "good" : ""],
      ["Verdict", context.checksTotal === 0 ? "running" : context.checksPassed ? "safe to start" : "refused"],
    ]),
    block(
      heading("What applying does"),
      text("BootShift copies its recovery system onto the EFI partition and points the next boot at it. Your disk is not touched yet."),
      text("The work happens during the restart you choose, before any operating system starts."),
    ),
  ];
}

function restartBlocks(): HTMLElement[] {
  return [
    heading("If the power goes out"),
    text("Turn the computer back on. BootShift reads its journal, finds the last chunk the drive confirmed, and carries on from there."),
    text("A half-written chunk is simply redone: a chunk is never larger than the distance it moves, so it can never overwrite its own source."),
    block(
      heading("What never happens"),
      facts([
        ["Table before data", "never"],
        ["Unverified chunk", "never journaled"],
        ["Boot into a half-moved disk", "blocked"],
      ]),
    ),
  ];
}

function doneBlocks(context: Context): HTMLElement[] {
  return [
    heading("Kept on the EFI partition"),
    facts([
      ["Partition table backup", "saved"],
      ["Move journal", "saved"],
      ["Recovery boot redirect", "removed"],
    ]),
    block(
      heading("If something looks wrong"),
      text(
        context.isDemo
          ? "In the real app, undo runs the same plan in reverse while the freed space is still empty."
          : "Undo runs the same plan in reverse while the freed space is still empty.",
      ),
    ),
  ];
}

function heading(text: string): HTMLElement {
  const element = document.createElement("h2");
  element.textContent = text;
  return element;
}

function text(content: string): HTMLElement {
  const element = document.createElement("p");
  element.className = "rtext";
  element.textContent = content;
  return element;
}

function block(...children: HTMLElement[]): HTMLElement {
  const element = document.createElement("div");
  element.className = "rblock";
  element.append(...children);
  return element;
}

function facts(rows: (readonly [string, string] | readonly [string, string, string])[]): HTMLElement {
  const list = document.createElement("div");
  list.append(
    ...rows.map(([key, value, tone]) => {
      const row = document.createElement("div");
      row.className = "rfact";
      const k = document.createElement("span");
      k.className = "k";
      k.textContent = key;
      const v = document.createElement("span");
      v.className = tone ? `v ${tone}` : "v";
      v.textContent = value;
      row.append(k, v);
      return row;
    }),
  );
  return list;
}
