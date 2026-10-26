// The gutter between the "Now" bar and the "After" bar. Every partition that moves gets a
// dimension line spanning exactly the distance it travels, arrowhead at the destination, the
// way a drawing marks a measurement. One line per moved partition, stacked, so none overlap.
// Nothing moved means nothing is drawn.

import { formatBytes, sectorsToBytes } from "./format";
import type { Layout, Target } from "./types";

const LANE_REM = 1.5;
/** Room above the first line for its label to sit clear of the bar overhead. */
const LANE_TOP_REM = 1.45;

export function renderDelta(container: HTMLElement, layout: Layout, target: Target): void {
  const usable = layout.geometry.usable;
  const share = (sector: number) => ((sector - usable.start) / usable.len) * 100;

  const moves = layout.partitions.flatMap((partition) => {
    const to = target[partition.unique_guid];
    if (!to || to.start === partition.extent.start) return [];
    return [{ partition, from: partition.extent.start, to: to.start }];
  });

  container.style.height = moves.length === 0 ? "" : `${moves.length * LANE_REM + 1.1}rem`;
  container.replaceChildren(
    ...moves.map(({ partition, from, to }, lane) => {
      const line = document.createElement("div");
      line.className = `shift ${to > from ? "right" : "left"}`;
      line.style.left = `${share(Math.min(from, to))}%`;
      line.style.width = `${Math.max(Math.abs(share(to) - share(from)), 0.3)}%`;
      line.style.top = `${lane * LANE_REM + LANE_TOP_REM}rem`;

      const tip = document.createElement("span");
      tip.className = "tip";
      const amount = document.createElement("span");
      amount.className = "amount";
      amount.textContent = `${partition.name || "partition " + partition.number} moves ${formatBytes(
        sectorsToBytes(layout, Math.abs(to - from)),
      )}`;
      line.append(tip, amount);
      return line;
    }),
  );
}
