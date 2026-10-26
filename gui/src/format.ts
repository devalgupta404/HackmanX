import type { Content, Layout, Operation, Partition } from "./types";

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${UNITS[unit]}`;
}

export function sectorsToBytes(layout: Layout, sectors: number): number {
  return sectors * layout.geometry.sector_size;
}

export function contentName(content: Content): string {
  const names: Record<Content, string> = {
    EfiSystem: "EFI system",
    Ext4: "ext4",
    Ntfs: "NTFS",
    Btrfs: "Btrfs",
    Xfs: "XFS",
    Swap: "swap",
    BitLocker: "BitLocker",
    Opaque: "unrecognised",
  };
  return names[content];
}

export function partitionLabel(partition: Partition): string {
  return partition.name
    ? `partition ${partition.number} "${partition.name}"`
    : `partition ${partition.number} (${contentName(partition.content)})`;
}

/** One plan step in plain words, matching the command line's wording. */
export function describeOperation(layout: Layout, operation: Operation): string {
  const size = (sectors: number) => formatBytes(sectorsToBytes(layout, sectors));
  const name = (guid: string) => {
    const partition = layout.partitions.find((p) => p.unique_guid === guid);
    return partition ? partitionLabel(partition) : guid;
  };
  if ("Shrink" in operation) return `Shrink ${name(operation.Shrink.partition)} to ${size(operation.Shrink.len)}`;
  if ("Move" in operation) {
    const { partition, to, copy_estimate } = operation.Move;
    return `Move ${name(partition)} to ${size(to)} (copies about ${size(copy_estimate)})`;
  }
  if ("Grow" in operation) return `Grow ${name(operation.Grow.partition)} to ${size(operation.Grow.len)}`;
  if ("RemoveSwap" in operation) return `Set aside swap ${name(operation.RemoveSwap.partition)}`;
  const swap = operation.RecreateSwap.partition;
  return `Recreate swap ${partitionLabel(swap)} at ${size(swap.extent.start)}`;
}
