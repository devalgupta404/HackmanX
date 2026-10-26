// The JSON the helper service speaks. Positions and lengths are in sectors.

export type Guid = string;

export interface Extent {
  start: number;
  len: number;
}

export type Content =
  | "EfiSystem"
  | "Ext4"
  | "Ntfs"
  | "Btrfs"
  | "Xfs"
  | "Swap"
  | "BitLocker"
  | "Opaque";

export interface Partition {
  number: number;
  unique_guid: Guid;
  type_guid: Guid;
  name: string;
  attributes: number;
  extent: Extent;
  content: Content;
  used: number;
  min_len: number | null;
}

export interface Geometry {
  sector_size: number;
  usable: Extent;
  alignment: number;
}

export interface Layout {
  geometry: Geometry;
  partitions: Partition[];
}

/** Where each partition should end up, keyed by its unique GUID. */
export type Target = Record<Guid, Extent>;

export interface Refusal {
  reason: string;
  fix: string;
}

export type Reply<T> =
  | { result: "ok"; value: T }
  | { result: "refused"; refusals: Refusal[] };

export interface DiskSummary {
  path: string;
  size: number;
  transport: string | null;
  removable: boolean;
  partition_table: string | null;
}

export interface Probed {
  disk: string;
  size: number;
  layout: Layout;
  target: Target;
}

export type Operation =
  | { Shrink: { partition: Guid; len: number } }
  | { Move: { partition: Guid; from: number; to: number; len: number; copy_estimate: number } }
  | { Grow: { partition: Guid; len: number } }
  | { RemoveSwap: { partition: Guid } }
  | { RecreateSwap: { partition: Partition } };

export interface Planned {
  operations: Operation[];
  copy_bytes: number;
  unavailable: Refusal[];
}

export interface Applied {
  restart_to_begin: boolean;
}
