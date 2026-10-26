// Talking to the helper service. In the desktop app every call goes through Tauri to the
// root helper over D-Bus; in a plain browser the demo backend stands in, for development.

import { invoke } from "@tauri-apps/api/core";

import { demoBackend } from "./demo";
import type { Applied, DiskSummary, Planned, Probed, Reply, Target } from "./types";

export interface Backend {
  /** True when showing made-up data instead of a real disk. */
  readonly isDemo: boolean;
  listDisks(): Promise<Reply<DiskSummary[]>>;
  probe(disk: string): Promise<Reply<Probed>>;
  plan(disk: string, target: Target): Promise<Reply<Planned>>;
  check(disk: string, target: Target): Promise<Reply<null>>;
  apply(disk: string, target: Target): Promise<Reply<Applied>>;
}

export function chooseBackend(): Backend {
  return "__TAURI_INTERNALS__" in window ? tauriBackend() : demoBackend();
}

function tauriBackend(): Backend {
  const call = async <T>(command: string, args: Record<string, unknown> = {}): Promise<Reply<T>> =>
    JSON.parse(await invoke<string>(command, args)) as Reply<T>;
  const withTarget = (disk: string, target: Target) => ({ disk, target: JSON.stringify(target) });
  return {
    isDemo: false,
    listDisks: () => call("list_disks"),
    probe: (disk) => call("probe", { disk }),
    plan: (disk, target) => call("plan", withTarget(disk, target)),
    check: (disk, target) => call("check", withTarget(disk, target)),
    apply: (disk, target) => call("apply", withTarget(disk, target)),
  };
}
