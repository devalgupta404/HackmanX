// Talking to the helper service. In the desktop app every call goes through Tauri to the
// root helper over D-Bus; in a plain browser the demo backend stands in, for development.

import { demoBackend } from "./demo";
import type { Applied, CheckResult, DiskSummary, Planned, Probed, Reply, Target } from "./types";

export interface Backend {
  /** True when showing made-up data instead of a real disk. */
  readonly isDemo: boolean;
  listDisks(): Promise<Reply<DiskSummary[]>>;
  probe(disk: string): Promise<Reply<Probed>>;
  plan(disk: string, target: Target): Promise<Reply<Planned>>;
  check(disk: string, target: Target): Promise<Reply<CheckResult[]>>;
  apply(disk: string, target: Target): Promise<Reply<Applied>>;
}

// Tauri injects this global into its webview; `@tauri-apps/api/core`'s invoke() is a
// one-line passthrough to it. Calling it directly keeps the browser build free of any
// Tauri import: a static import here crashed the whole dev page when the package could
// not be resolved, leaving nothing clickable in demo mode.
interface TauriInternals {
  invoke(command: string, args?: Record<string, unknown>): Promise<string>;
}

function tauriInternals(): TauriInternals | undefined {
  return (window as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__;
}

export function chooseBackend(): Backend {
  const internals = tauriInternals();
  return internals ? tauriBackend(internals) : demoBackend();
}

function tauriBackend(internals: TauriInternals): Backend {
  const call = async <T>(command: string, args: Record<string, unknown> = {}): Promise<Reply<T>> =>
    JSON.parse(await internals.invoke(command, args)) as Reply<T>;
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
