// The partition table under the disk. Small partitions are unreadable inside the bar, so this
// is where every partition can be read, compared and picked.

import { canMove, isChanged } from "./editing";
import { contentName, formatBytes, sectorsToBytes } from "./format";
import type { Guid, Layout, Target } from "./types";

export function renderTable(
  container: HTMLElement,
  layout: Layout,
  target: Target,
  selected: Guid | undefined,
  onPick: (guid: Guid) => void,
): void {
  const size = (sectors: number) => formatBytes(sectorsToBytes(layout, sectors));

  const header = document.createElement("div");
  header.className = "trow labels";
  header.append(
    cell("", ""),
    cell("", "Partition"),
    cell("", "Contents"),
    cell("", "Size"),
    cell("", "Used"),
    cell("t-change quiet", "Change"),
  );

  container.replaceChildren(
    header,
    ...layout.partitions.map((partition) => {
      const extent = target[partition.unique_guid] ?? partition.extent;
      const row = document.createElement("button");
      row.type = "button";
      row.className = "trow";
      if (partition.unique_guid === selected) row.classList.add("selected");

      const swatch = document.createElement("div");
      swatch.className = `sw content-${partition.content.toLowerCase()}`;

      const grew = extent.len - partition.extent.len;
      const moved = extent.start !== partition.extent.start;
      const change = !isChanged(layout, target, partition.unique_guid)
        ? canMove(partition.content)
          ? "unchanged"
          : "locked"
        : [grew !== 0 ? `${grew > 0 ? "+" : "−"}${size(Math.abs(grew))}` : "", moved ? "moves" : ""]
            .filter(Boolean)
            .join(", ");

      row.append(
        swatch,
        cell("t-name", partition.name || contentName(partition.content)),
        cell("t-num", contentName(partition.content)),
        cell("t-num", size(extent.len)),
        cell("t-num", partition.used > 0 ? size(partition.used) : "empty"),
        cell(`t-change${change === "unchanged" || change === "locked" ? " quiet" : ""}`, change),
      );
      row.classList.add("pickable");
      row.addEventListener("click", () => onPick(partition.unique_guid));
      return row;
    }),
  );
}

function cell(className: string, text: string): HTMLDivElement {
  const element = document.createElement("div");
  if (className) element.className = className;
  element.textContent = text;
  return element;
}
