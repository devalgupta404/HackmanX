import { describe, expect, it } from "vitest";

import { canMove, isChanged, minLen, moveTo, setEnd, setStart } from "./editing";
import type { Content, Layout, Partition, Target } from "./types";

const MIB = 2048;

function partition(number: number, content: Content, startMib: number, lenMib: number, minMib: number | null): Partition {
  return {
    number,
    unique_guid: `guid-${number}`,
    type_guid: "type",
    name: `part${number}`,
    attributes: 0,
    extent: { start: startMib * MIB, len: lenMib * MIB },
    content,
    used: (minMib ?? lenMib) * MIB,
    min_len: minMib === null ? null : minMib * MIB,
  };
}

/** A 1000 MiB disk: ESP, Windows (NTFS), Linux (ext4) with free space after it. */
function layout(): Layout {
  return {
    geometry: { sector_size: 512, usable: { start: MIB, len: 998 * MIB }, alignment: MIB },
    partitions: [
      partition(1, "EfiSystem", 1, 100, null),
      partition(2, "Ntfs", 101, 400, 150),
      partition(3, "Ext4", 501, 300, 100),
    ],
  };
}

function targetOf(l: Layout): Target {
  return Object.fromEntries(l.partitions.map((p) => [p.unique_guid, { ...p.extent }]));
}

describe("moving", () => {
  it("slides a partition into free space, snapped to 1 MiB", () => {
    const l = layout();
    const moved = moveTo(l, targetOf(l), "guid-3", 600 * MIB + 700);
    expect(moved["guid-3"]).toEqual({ start: 600 * MIB, len: 300 * MIB });
  });

  it("stops at the neighbour instead of overlapping it", () => {
    const l = layout();
    const moved = moveTo(l, targetOf(l), "guid-3", 50 * MIB);
    expect(moved["guid-3"]?.start).toBe(501 * MIB);
  });

  it("stops at the end of the disk", () => {
    const l = layout();
    const moved = moveTo(l, targetOf(l), "guid-3", 900 * MIB);
    expect(moved["guid-3"]?.start).toBe(699 * MIB);
  });

  it("never moves the EFI system partition", () => {
    const l = layout();
    const target = targetOf(l);
    expect(moveTo(l, target, "guid-1", 300 * MIB)).toBe(target);
    expect(canMove("EfiSystem")).toBe(false);
  });
});

describe("resizing the end", () => {
  it("grows into free space up to the end of the disk", () => {
    const l = layout();
    const grown = setEnd(l, targetOf(l), "guid-3", 5000 * MIB);
    expect(grown["guid-3"]).toEqual({ start: 501 * MIB, len: 498 * MIB });
  });

  it("shrinks no further than the filesystem allows", () => {
    const l = layout();
    const shrunk = setEnd(l, targetOf(l), "guid-2", 120 * MIB);
    expect(shrunk["guid-2"]).toEqual({ start: 101 * MIB, len: 150 * MIB });
  });

  it("can't grow into the next partition", () => {
    const l = layout();
    const target = targetOf(l);
    expect(setEnd(l, target, "guid-2", 700 * MIB)).toBe(target);
  });

  it("can't shrink a partition whose minimum size is unknown", () => {
    const l = layout();
    l.partitions[2] = partition(3, "Ext4", 501, 300, null);
    const target = targetOf(l);
    expect(setEnd(l, target, "guid-3", 600 * MIB)).toBe(target);
  });
});

describe("resizing the start", () => {
  it("grows left into space freed by shrinking the neighbour", () => {
    const l = layout();
    const freed = setEnd(l, targetOf(l), "guid-2", 251 * MIB);
    const grown = setStart(l, freed, "guid-3", 251 * MIB);
    expect(grown["guid-3"]).toEqual({ start: 251 * MIB, len: 550 * MIB });
  });

  it("keeps at least the minimum size when the start moves right", () => {
    const l = layout();
    const shrunk = setStart(l, targetOf(l), "guid-3", 790 * MIB);
    expect(shrunk["guid-3"]).toEqual({ start: 701 * MIB, len: 100 * MIB });
  });
});

describe("helpers", () => {
  it("swap may shrink to one alignment unit regardless of contents", () => {
    const swap = partition(4, "Swap", 801, 64, null);
    expect(minLen(layout(), swap)).toBe(MIB);
  });

  it("knows which partitions the user changed", () => {
    const l = layout();
    const moved = moveTo(l, targetOf(l), "guid-3", 600 * MIB);
    expect(isChanged(l, moved, "guid-3")).toBe(true);
    expect(isChanged(l, moved, "guid-2")).toBe(false);
  });
});
