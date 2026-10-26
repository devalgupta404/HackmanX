// The disk drawn as a bar of partitions. In edit mode, dragging a partition moves it and
// dragging its edges resizes it; the arrow keys do the same in 1 GiB steps (Shift resizes).

import { canMove, moveTo, setEnd, setStart } from "./editing";
import { contentName, formatBytes, sectorsToBytes } from "./format";
import type { Extent, Guid, Layout, Partition, Target } from "./types";

type Drag = "move" | "start" | "end";

const KEY_STEP_BYTES = 1024 ** 3;
/** Partitions smaller than this share of the disk are drawn above their neighbours. */
const TINY_FRACTION = 0.01;
/** The room the leader lines take under the callout capsules, in pixels. */
const LEADER_HEIGHT = 8;
const CAPSULE_GAP = 6;

export class DiskBar {
  private readonly bar: HTMLElement;
  private readonly flags: HTMLElement;
  private readonly leaders: SVGSVGElement;
  private readonly blocks = new Map<Guid, HTMLElement>();
  private target: Target;

  constructor(
    container: HTMLElement,
    private readonly layout: Layout,
    target: Target,
    private readonly onChange?: (target: Target) => void,
    private readonly onSelect?: (guid: Guid) => void,
  ) {
    this.target = target;
    this.bar = document.createElement("div");
    this.bar.className = this.editable ? "disk-bar editable" : "disk-bar";
    for (const partition of layout.partitions) {
      const block = this.createBlock(partition);
      this.blocks.set(partition.unique_guid, block);
      this.bar.append(block);
    }
    this.flags = document.createElement("div");
    this.flags.className = "flags";
    this.leaders = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    this.leaders.setAttribute("class", "leaders");
    this.flags.append(this.leaders);

    const wrap = document.createElement("div");
    wrap.className = "bar-wrap";
    wrap.append(this.flags, this.bar);
    container.replaceChildren(wrap);
    if (this.editable) container.append(buildScale(layout));
    this.update(target);
    // The capsules are laid out in pixels, so they follow the bar when the window resizes.
    new ResizeObserver(() => this.renderFlags()).observe(this.bar);
  }

  update(target: Target): void {
    this.target = target;
    const usable = this.layout.geometry.usable;
    for (const partition of this.layout.partitions) {
      const block = this.blocks.get(partition.unique_guid);
      const extent = target[partition.unique_guid];
      if (!block || !extent) continue;
      block.style.left = `${((extent.start - usable.start) / usable.len) * 100}%`;
      block.style.width = `${(extent.len / usable.len) * 100}%`;
      block.classList.toggle("tiny", extent.len / usable.len < TINY_FRACTION);
      const size = block.querySelector(".size");
      if (size) size.textContent = formatBytes(sectorsToBytes(this.layout, extent.len));
      const used = block.querySelector<HTMLElement>(".used");
      if (used) used.style.width = `${Math.min(100, (partition.used / extent.len) * 100)}%`;
    }
    this.renderFlags();
  }

  /** A partition too thin to hold its own label gets a capsule above the bar, joined to its
   *  sliver by a leader line. Capsules are nudged apart so two slivers side by side still
   *  read as two partitions. */
  private renderFlags(): void {
    const width = this.bar.clientWidth;
    const slivers = !this.editable
      ? []
      : this.layout.partitions.filter((partition) =>
          this.blocks.get(partition.unique_guid)?.classList.contains("tiny"),
        );
    this.flags.hidden = slivers.length === 0;
    if (width === 0 || slivers.length === 0) {
      this.leaders.replaceChildren();
      return;
    }

    const capsules = slivers.map((partition) => {
      const block = this.blocks.get(partition.unique_guid);
      const extent = this.target[partition.unique_guid] ?? partition.extent;
      const capsule = this.createCapsule(partition, extent);
      return { capsule, anchor: (block?.offsetLeft ?? 0) + (block?.offsetWidth ?? 0) / 2 };
    });
    this.flags.replaceChildren(this.leaders, ...capsules.map((item) => item.capsule));

    let placed = 0;
    const lines: { from: number; to: number }[] = [];
    for (const { capsule, anchor } of capsules) {
      const size = capsule.offsetWidth;
      const left = Math.min(Math.max(anchor - size / 2, placed), width - size);
      capsule.style.left = `${left}px`;
      placed = left + size + CAPSULE_GAP;
      lines.push({ from: left + size / 2, to: anchor });
    }

    this.leaders.setAttribute("viewBox", `0 0 ${width} ${LEADER_HEIGHT}`);
    this.leaders.replaceChildren(
      ...lines.map(({ from, to }) => {
        const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
        line.setAttribute("x1", String(from));
        line.setAttribute("y1", "0");
        line.setAttribute("x2", String(to));
        line.setAttribute("y2", String(LEADER_HEIGHT));
        return line;
      }),
    );
  }

  private createCapsule(partition: Partition, extent: Extent): HTMLElement {
    const capsule = document.createElement("div");
    capsule.className = "flag";
    const dot = document.createElement("span");
    dot.className = `dot content-${partition.content.toLowerCase()}`;
    const name = document.createElement("span");
    name.className = "fname";
    name.textContent = partition.name || contentName(partition.content);
    const size = document.createElement("span");
    size.className = "fsize";
    size.textContent = formatBytes(sectorsToBytes(this.layout, extent.len));
    capsule.append(dot, name, size);
    capsule.title = `${name.textContent}, ${contentName(partition.content)}, ${size.textContent}`;
    if (this.editable && canMove(partition.content)) {
      capsule.classList.add("pickable");
      capsule.tabIndex = 0;
      capsule.addEventListener("click", () => {
        this.blocks.get(partition.unique_guid)?.focus();
        this.onSelect?.(partition.unique_guid);
      });
    }
    return capsule;
  }

  /** Rings the chosen partition in both bars, so the table and the disk agree. */
  select(guid: Guid | undefined): void {
    for (const [other, block] of this.blocks) block.classList.toggle("selected", other === guid);
  }

  private get editable(): boolean {
    return this.onChange !== undefined;
  }

  private createBlock(partition: Partition): HTMLElement {
    const block = document.createElement("div");
    block.className = `partition content-${partition.content.toLowerCase()}`;
    const movable = this.editable && canMove(partition.content);
    if (this.editable && !movable) block.classList.add("locked");
    if (movable) block.classList.add("movable");

    const used = document.createElement("div");
    used.className = "used";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = partition.name || contentName(partition.content);
    const size = document.createElement("span");
    size.className = "size";
    block.append(used, name, size);
    block.title = `${partition.name || "partition " + partition.number}, ${contentName(partition.content)}`;
    block.addEventListener("click", () => this.onSelect?.(partition.unique_guid));

    if (movable) {
      block.tabIndex = 0;
      block.setAttribute("role", "slider");
      block.setAttribute("aria-label", `${name.textContent}: drag to move, drag edges to resize`);
      for (const edge of ["start", "end"] as const) {
        const handle = document.createElement("div");
        handle.className = `handle ${edge}`;
        handle.title = `Drag to resize ${name.textContent}`;
        handle.addEventListener("pointerdown", (event) => this.beginDrag(event, partition.unique_guid, edge));
        block.append(handle);
      }
      const grab = document.createElement("div");
      grab.className = "grab";
      block.append(grab);
      block.addEventListener("pointerdown", (event) => this.beginDrag(event, partition.unique_guid, "move"));
      block.addEventListener("keydown", (event) => this.onKey(event, partition.unique_guid));
    }
    return block;
  }

  private beginDrag(event: PointerEvent, guid: Guid, kind: Drag): void {
    event.stopPropagation();
    event.preventDefault();
    const element = event.currentTarget as HTMLElement;
    const origin = this.target;
    const from = origin[guid];
    if (!from) return;
    const sectorsPerPixel = this.layout.geometry.usable.len / this.bar.clientWidth;
    const startX = event.clientX;
    element.setPointerCapture(event.pointerId);
    this.bar.classList.add(kind === "move" ? "moving" : "resizing");

    const onMove = (move: PointerEvent) => {
      const delta = (move.clientX - startX) * sectorsPerPixel;
      this.change(this.applyDrag(origin, guid, kind, from.start, from.len, delta));
    };
    const onUp = () => {
      this.bar.classList.remove("moving", "resizing");
      element.removeEventListener("pointermove", onMove);
      element.removeEventListener("pointerup", onUp);
      element.removeEventListener("pointercancel", onUp);
    };
    element.addEventListener("pointermove", onMove);
    element.addEventListener("pointerup", onUp);
    element.addEventListener("pointercancel", onUp);
  }

  private onKey(event: KeyboardEvent, guid: Guid): void {
    const direction = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    const current = this.target[guid];
    if (direction === 0 || !current) return;
    event.preventDefault();
    const step = (direction * KEY_STEP_BYTES) / this.layout.geometry.sector_size;
    const kind: Drag = event.shiftKey ? "end" : "move";
    this.change(this.applyDrag(this.target, guid, kind, current.start, current.len, step));
  }

  private applyDrag(origin: Target, guid: Guid, kind: Drag, start: number, len: number, delta: number): Target {
    switch (kind) {
      case "move":
        return moveTo(this.layout, origin, guid, start + delta);
      case "start":
        return setStart(this.layout, origin, guid, start + delta);
      case "end":
        return setEnd(this.layout, origin, guid, start + len + delta);
    }
  }

  private change(target: Target): void {
    if (target === this.target) return;
    this.update(target);
    this.onChange?.(target);
  }
}

/** A measuring scale under the editable bar: tick marks, with 0, midpoint and full size. */
function buildScale(layout: Layout): HTMLElement {
  const scale = document.createElement("div");
  scale.className = "scale";
  const ticks = document.createElement("div");
  ticks.className = "scale-ticks";
  const labels = document.createElement("div");
  labels.className = "scale-labels";
  const total = sectorsToBytes(layout, layout.geometry.usable.len);
  for (const bytes of [0, total / 2, total]) {
    const label = document.createElement("span");
    label.textContent = bytes === 0 ? "0" : formatBytes(bytes);
    labels.append(label);
  }
  scale.append(ticks, labels);
  return scale;
}
