# Known issues

What is wrong, missing or misleading in the code right now, found in the code audit of
2026-10-26. `task.md` stays the ordered work plan; each issue here names the task that closes
it. Remove an issue in the same change that fixes it.

## Release blockers

- **I-01 `xtask` is an empty stub.** `xtask/src/main.rs` is `fn main() {}`, so the
  `cargo xtask fixture`, `vm-test` and `crash-suite` commands that `CLAUDE.md` tells every
  contributor to run do not exist. Blocks tasks 2.4, 2.5, 8.5 and all of Phase 11.
- **I-02 The boot path has never run.** `apply`, the GRUB redirect, recovery as PID 1 and
  `resume` are written and tested against fakes, but have never executed in a VM. Every bit of
  confidence so far comes from simulated disks. Tasks 0.5, 10.2, 10b.2, 2.5.
- **I-03 Guarded runs have no kernel transport.** The undo-log core (`disk::guard`,
  `disk::UndoLog`, `disk::roll_back`) is crash-tested, but nothing connects `resize2fs` or
  `ntfsresize` to it, so at runtime ext4/NTFS shrink and NTFS grow refuse with "this needs a
  guarded run, which this build doesn't have yet". Until it lands, BootShift can move anything
  and grow ext4/Btrfs/XFS, but can't take space *from* an NTFS or ext4 partition. Tasks 0.6, 7.3.
- **I-06 Windows survival is unproven.** Nobody has verified that Windows 11 boots after its
  partition moves, or that WinRE survives a recovery-partition move. This is the project's
  biggest external risk. Tasks 0.2, 0.3.

## Degraded behaviour

- **I-04 The desktop app does not exist.** `gui/src-tauri` is absent; `chooseBackend()` can
  only ever take the demo branch, so the GUI has never talked to `bootshiftd`, and the helper
  has never been called by anything but its own smoke test. Task 13.5.
- **I-05 No progress reporting.** `move_range` has no observer hook, and the recovery screen
  is two `eprintln` lines followed by silence for the whole copy. Neither recovery nor a future
  GUI can show percent or time left without one. Task 10.3 (needs a `ProgressSink` trait in the
  engine).
- **I-07 `accept_unreadable` is unreachable.** The copier supports copying past unreadable
  sectors after explicit consent, but no CLI flag, recovery prompt or GUI control can turn it
  on. An EIO is a hard stop with no accept path. Task 5.4.
- **I-08 systemd-boot is not supported yet**, though section 0 scopes it in (without Secure
  Boot). Preflight refuses it. Task 10.5.
- **I-11 Btrfs and XFS moves copy every byte.** Their used-ranges readers aren't written, so
  the scope falls back to `Everything`. Correct but slow, and the estimate shows it honestly.
  Tasks 7.6, 7.7.

## Paper cuts

- **I-09 The recovery error screen waits for Enter forever.** If the keyboard modules failed
  to load, the machine sits silently instead of powering off. Needs a timeout. Task 10.2.
- **I-10 The helper's work directory is never cleaned.** `bootshiftd` builds initramfs
  artifacts in a fixed `$TMPDIR/bootshiftd` that survives failed runs. Task 12.4.
- **I-12 No cancel or undo.** Task 8.6.
- **I-13 No dm-linear staging check** before the first table write. Task 8.3.
- **I-14 The GUI stops at "restart later".** The restart screen and post-restart summary now
  exist in the frontend with a demo simulation, but nothing reads the real journal for the
  summary, and the table-backup download is missing. Tasks 13.3, 13.4.
- **I-15 No packages, no `docs/design.md`, no demo script.** Tasks 14.2, 14.3, 14.4.

## Fixed

- **I-16 `CLAUDE.md` told contributors to run commands that don't exist** (`cargo xtask …`,
  see I-01). Fixed 2026-10-26: the commands are now marked as not yet built.
