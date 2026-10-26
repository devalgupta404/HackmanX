// Made-up data for working on the interface in a browser. Nothing here touches a disk, and the
// "plan" only lists what changed; the real planner runs in the helper service.

import type { Backend } from "./backend";
import type { Layout, Operation, Target } from "./types";

const MIB = 2048;
const GIB = 1024 * MIB;

function demoLayout(): Layout {
  const partition = (
    number: number,
    content: Layout["partitions"][number]["content"],
    name: string,
    start: number,
    len: number,
    used: number,
    min: number | null,
  ) => ({
    number,
    unique_guid: `0000000${number}-0000-4000-8000-00000000000${number}`,
    type_guid: "00000000-0000-0000-0000-000000000000",
    name,
    attributes: 0,
    extent: { start, len },
    content,
    used,
    min_len: min,
  });
  return {
    geometry: { sector_size: 512, usable: { start: MIB, len: 476 * GIB }, alignment: MIB },
    partitions: [
      partition(1, "EfiSystem", "EFI system partition", MIB, 260 * MIB, 40 * MIB, null),
      partition(2, "Opaque", "Microsoft reserved", 261 * MIB, 16 * MIB, 16 * MIB, null),
      partition(3, "Ntfs", "Windows", 277 * MIB, 200 * GIB, 60 * GIB, 61 * GIB),
      partition(4, "Ntfs", "Windows recovery", 277 * MIB + 200 * GIB, 800 * MIB, 500 * MIB, 600 * MIB),
      partition(5, "Ext4", "Linux", 1077 * MIB + 200 * GIB, 250 * GIB, 90 * GIB, 92 * GIB),
      partition(6, "Swap", "", 1077 * MIB + 450 * GIB, 8 * GIB, 0, 0),
    ],
  };
}

function currentTarget(layout: Layout): Target {
  return Object.fromEntries(layout.partitions.map((p) => [p.unique_guid, { ...p.extent }]));
}

function listChanges(layout: Layout, target: Target): Operation[] {
  const operations: Operation[] = [];
  for (const p of layout.partitions) {
    const to = target[p.unique_guid];
    if (!to) continue;
    if (to.len < p.extent.len) operations.push({ Shrink: { partition: p.unique_guid, len: to.len } });
    if (to.start !== p.extent.start) {
      const len = Math.min(to.len, p.extent.len);
      operations.push({
        Move: { partition: p.unique_guid, from: p.extent.start, to: to.start, len, copy_estimate: Math.min(p.used, len) },
      });
    }
    if (to.len > p.extent.len) operations.push({ Grow: { partition: p.unique_guid, len: to.len } });
  }
  return operations;
}

export function demoBackend(): Backend {
  const layout = demoLayout();
  return {
    isDemo: true,
    listDisks: async () => ({
      result: "ok",
      value: [{ path: "/dev/demo", size: 512 * 1024 ** 3, transport: "nvme", removable: false, partition_table: "gpt" }],
    }),
    probe: async (disk) => ({
      result: "ok",
      value: { disk, size: 512 * 1024 ** 3, layout, target: currentTarget(layout) },
    }),
    plan: async (_disk, target) => {
      const operations = listChanges(layout, target);
      const copied = operations.reduce((sum, op) => sum + ("Move" in op ? op.Move.copy_estimate : 0), 0);
      return { result: "ok", value: { operations, copy_bytes: copied * 512, unavailable: [] } };
    },
    check: async () => ({
      result: "refused",
      refusals: [{ reason: "This is demo data, not a real disk.", fix: "Run BootShift as the desktop app to check a real disk." }],
    }),
    apply: async () => ({
      result: "refused",
      refusals: [{ reason: "This is demo data, not a real disk.", fix: "Run BootShift as the desktop app to change a real disk." }],
    }),
  };
}
