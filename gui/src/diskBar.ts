// The disk drawn as a bar of partitions. In edit mode, dragging a partition moves it and
// dragging its edges resizes it; the arrow keys do the same in 1 GiB steps (Shift resizes).

import { canMove, moveTo, setEnd, setStart } from "./editing";
import { contentName, formatBytes, sectorsToBytes } from "./format";
import type { Guid, Layout, Partition, Target } from "./types";

type Drag = "move" | "start" | "end";

const KEY_STEP_BYTES = 1024 ** 3;
/** Partitions smaller than this share of the disk are drawn above their neighbours. */
const TINY_FRACTION = 0.01;

export class DiskBar {
  private readonly bar: HTMLElement;
  private readonly blocks = new Map<Guid, HTMLElement>();
  private target: Target;

  constructor(
    container: HTMLElement,
    private readonly layout: Layout,
    target: Target,
    private readonly onChange?: (target: Target) => void,
  ) {
    this.target = target;
    this.bar = document.createElement("div");
    this.bar.className = "disk-bar";
    for (const partition of layout.partitions) {
      const block = this.createBlock(partition);
      this.blocks.set(partition.unique_guid, block);
      this.bar.append(block);
    }
    container.replaceChildren(this.bar);
    this.update(target);
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
  }

  private get editable(): boolean {
    return this.onChange !== undefined;
  }

  private createBlock(partition: Partition): HTMLElement {
    const block = document.createElement("div");
    block.className = `partition content-${partition.content.toLowerCase()}`;
    const movable = canMove(partition.content);
    if (!movable) block.classList.add("locked");

    const used = document.createElement("div");
    used.className = "used";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = partition.name || contentName(partition.content);
    const size = document.createElement("span");
    size.className = "size";
    block.append(used, name, size);
    block.title = `${partition.name || "partition " + partition.number} · ${contentName(partition.content)}`;

    if (this.editable && movable) {
      block.tabIndex = 0;
      block.setAttribute("role", "slider");
      block.setAttribute("aria-label", `${name.textContent}: drag to move, drag edges to resize`);
      for (const edge of ["start", "end"] as const) {
        const handle = document.createElement("div");
        handle.className = `handle ${edge}`;
        handle.addEventListener("pointerdown", (event) => this.beginDrag(event, partition.unique_guid, edge));
        block.append(handle);
      }
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

    const onMove = (move: PointerEvent) => {
      const delta = (move.clientX - startX) * sectorsPerPixel;
      this.change(this.applyDrag(origin, guid, kind, from.start, from.len, delta));
    };
    const onUp = () => {
      element.removeEventListener("pointermove", onMove);
      element.removeEventListener("pointerup", onUp);
    };
    element.addEventListener("pointermove", onMove);
    element.addEventListener("pointerup", onUp);
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
