// How dragging changes the target layout. Every change is snapped to the disk's alignment and
// clamped so partitions never overlap, never leave the disk, and only shrink or grow when their
// contents allow it. These rules mirror `core::validate`, which the helper applies again.

import type { Content, Extent, Guid, Layout, Partition, Target } from "./types";

export function canMove(content: Content): boolean {
  return content !== "EfiSystem" && content !== "BitLocker";
}

export function canGrow(content: Content): boolean {
  return ["Ext4", "Ntfs", "Btrfs", "Xfs", "Swap"].includes(content);
}

export function canShrink(content: Content): boolean {
  return ["Ext4", "Ntfs", "Btrfs", "Swap"].includes(content);
}

/** The smallest the partition may become, in sectors. */
export function minLen(layout: Layout, partition: Partition): number {
  const alignment = layout.geometry.alignment;
  if (!canShrink(partition.content)) return partition.extent.len;
  if (partition.content === "Swap") return alignment;
  return alignUp(partition.min_len ?? partition.extent.len, alignment);
}

/** The largest the partition may become before neighbours are considered, in sectors. */
export function maxLen(partition: Partition): number {
  return canGrow(partition.content) ? Number.POSITIVE_INFINITY : partition.extent.len;
}

/** Slides a partition to start near `requestedStart`, staying between its neighbours. */
export function moveTo(layout: Layout, target: Target, guid: Guid, requestedStart: number): Target {
  const partition = find(layout, guid);
  const current = target[guid];
  if (!partition || !current || !canMove(partition.content)) return target;

  const alignment = layout.geometry.alignment;
  const { left, right } = neighbourBounds(layout, target, guid);
  const lowest = alignUp(left, alignment);
  const highest = alignDown(right - current.len, alignment);
  if (highest < lowest) return target;

  const start = clamp(snap(requestedStart, alignment), lowest, highest);
  return withExtent(target, guid, { start, len: current.len });
}

/** Moves the partition's end to near `requestedEnd`, keeping its start. */
export function setEnd(layout: Layout, target: Target, guid: Guid, requestedEnd: number): Target {
  const partition = find(layout, guid);
  const current = target[guid];
  if (!partition || !current) return target;

  const alignment = layout.geometry.alignment;
  const { right } = neighbourBounds(layout, target, guid);
  const shortest = current.start + minLen(layout, partition);
  const longest = Math.min(alignDown(right, alignment), current.start + maxLen(partition));
  const end = clamp(snap(requestedEnd, alignment), shortest, longest);
  if (end === current.start + current.len || longest < shortest) return target;
  return withExtent(target, guid, { start: current.start, len: end - current.start });
}

/** Moves the partition's start to near `requestedStart`, keeping its end. */
export function setStart(layout: Layout, target: Target, guid: Guid, requestedStart: number): Target {
  const partition = find(layout, guid);
  const current = target[guid];
  if (!partition || !current || !canMove(partition.content)) return target;

  const alignment = layout.geometry.alignment;
  const end = current.start + current.len;
  const { left } = neighbourBounds(layout, target, guid);
  const earliest = Math.max(alignUp(left, alignment), end - maxLen(partition));
  const latest = end - minLen(layout, partition);
  const start = clamp(snap(requestedStart, alignment), earliest, latest);
  if (start === current.start || latest < earliest) return target;
  return withExtent(target, guid, { start, len: end - start });
}

export function isChanged(layout: Layout, target: Target, guid: Guid): boolean {
  const before = find(layout, guid)?.extent;
  const after = target[guid];
  return !!before && !!after && (before.start !== after.start || before.len !== after.len);
}

/** The free space a partition may use: from the end of the one before it to the start of the
 * one after it, in the target layout. */
export function neighbourBounds(layout: Layout, target: Target, guid: Guid): { left: number; right: number } {
  const usable = layout.geometry.usable;
  const me = target[guid];
  let left = usable.start;
  let right = usable.start + usable.len;
  if (!me) return { left, right };
  for (const [other, extent] of Object.entries(target)) {
    if (other === guid) continue;
    const end = extent.start + extent.len;
    if (end <= me.start) left = Math.max(left, end);
    if (extent.start >= me.start + me.len) right = Math.min(right, extent.start);
  }
  return { left, right };
}

function find(layout: Layout, guid: Guid): Partition | undefined {
  return layout.partitions.find((p) => p.unique_guid === guid);
}

function withExtent(target: Target, guid: Guid, extent: Extent): Target {
  return { ...target, [guid]: extent };
}

function snap(value: number, alignment: number): number {
  return Math.round(value / alignment) * alignment;
}

function alignUp(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

function alignDown(value: number, alignment: number): number {
  return Math.floor(value / alignment) * alignment;
}

function clamp(value: number, lowest: number, highest: number): number {
  return Math.min(Math.max(value, lowest), highest);
}
