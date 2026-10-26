# BootShift

Rearrange the partitions on your disk by dragging them in a window. BootShift applies the change during one restart, with no USB stick, and a power cut at any point can't destroy your data.

**Status:** early development. Do not run on a disk you care about.

## How it works

- Every disk change happens in a small recovery system started from the EFI partition, before any operating system mounts anything.
- Data moves in place, chunk by chunk, so no spare disk or extra empty space is needed.
- Every step is recorded in a journal before and after it runs, so a power cut at any moment can be resumed.
- Anything BootShift can't handle safely is refused before a single byte on the disk changes.

## Repository layout

| Path | Contents |
|---|---|
| `crates/core` | Pure logic: layout model, planner, validation, journal format. No I/O. |
| `crates/disk` | Block device I/O, crash-simulating test disks, GPT, journal storage |
| `crates/fs` | Filesystem adapters (ext4, NTFS, Btrfs, swap) driving the standard tools |
| `crates/boot` | EFI partition files, GRUB redirect, boot order, recovery initramfs |
| `crates/engine` | Preflight checks, copier, step executor, verification |
| `crates/cli` | The `bootshift` binary, used in both the normal system and recovery boot |
| `crates/helper` | `bootshiftd`, the privileged D-Bus service |
| `gui/` | Tauri desktop app |
| `xtask/` | Developer tasks: fixtures, VM tests, crash suites |
| `packaging/` | D-Bus, systemd and polkit files |

## Contributing

- [`task.md`](task.md): the ordered work plan, and the research behind the design.
- [`CLAUDE.md`](CLAUDE.md): the safety rules, code style and testing rules all contributions follow.
