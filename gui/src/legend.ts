// The rows under the disk bar: one per partition, with its colour, contents and sizes, so
// small partitions that are hard to read inside the bar still have a place to be understood.

import { canMove } from "./editing";
import { contentName, formatBytes, sectorsToBytes } from "./format";
import type { Layout, Target } from "./types";

export function renderLegend(container: HTMLElement, layout: Layout, target: Target): void {
  container.replaceChildren(
    ...layout.partitions.map((partition) => {
      const extent = target[partition.unique_guid] ?? partition.extent;
      const row = document.createElement("div");
      row.className = "legend-row";

      const swatch = document.createElement("div");
      swatch.className = `sw content-${partition.content.toLowerCase()}`;

      const name = cell("l-name", partition.name || contentName(partition.content));
      const contents = cell("l-dim", contentName(partition.content));
      const size = cell("l-dim", formatBytes(sectorsToBytes(layout, extent.len)));
      const note = cell(
        "l-dim",
        !canMove(partition.content)
          ? "locked"
          : partition.used > 0
            ? `${formatBytes(sectorsToBytes(layout, partition.used))} used`
            : "empty",
      );
      row.append(swatch, name, contents, size, note);
      return row;
    }),
  );
}

function cell(className: string, text: string): HTMLDivElement {
  const element = document.createElement("div");
  element.className = className;
  element.textContent = text;
  return element;
}
