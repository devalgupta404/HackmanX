# BootShift

Rearrange the partitions on your disk by dragging them in a window. BootShift applies the change during one restart, with no USB stick, and a power cut at any point can't destroy your data.

**Status:** under active development and not yet tested on real hardware. Do not use it on a disk you care about.

## How it works

1. You drag partitions into the layout you want. BootShift shows every step and roughly how much it will copy.
2. It runs its safety checks, and refuses anything it can't do safely, saying how to fix it.
3. It writes a journal and a small recovery system to the EFI system partition, then asks you to restart.
4. The recovery system moves the data, chunk by chunk, before any operating system starts. It records every step in the journal, so if the power goes out, turning the computer back on carries on from where it stopped.
5. It puts normal booting back and restarts into your system.

## What it can do

| Partition holds | Move | Grow | Shrink |
|---|---|---|---|
| ext4 | yes | yes | not yet (needs guarded resizing) |
| NTFS (Windows) | yes | not yet | not yet |
| Btrfs | yes | yes | yes |
| XFS | yes | yes | never (XFS can't shrink) |
| swap | yes | yes | yes (recreated with the same UUID) |
| anything else (LVM, LUKS, …) | yes, byte for byte | no | no |
| EFI system partition | no: it holds the recovery system | no | no |

## What it refuses, and why

- **BitLocker:** an encrypted volume can't be read or resized. Turn off device encryption in Windows first (suspending isn't enough).
- **Hibernated Windows or Fast Startup:** Windows would resume over a changed disk. Turn off Fast Startup and shut down fully.
- **Legacy BIOS or MBR disks:** the recovery system starts from the EFI system partition.
- **USB disks:** USB adapters often ignore the commands that make writes durable.
- **A failing disk, a laptop on battery, too little room on the EFI system partition, filesystem errors.**

## If something goes wrong

- **The power went out:** turn the computer back on. BootShift continues by itself.
- **The recovery screen shows an error:** it powers off without starting any operating system, so nothing half-finished is ever used. Your data is where the journal says.
- **You want to finish from a live USB:** boot any Linux live USB with BootShift on it and run:

  ```
  sudo bootshift resume /dev/<disk> "$(cat /path/to/esp/EFI/bootshift/recovery-args.txt)"
  ```
- **Your old partition table:** BootShift saves it to `EFI/bootshift/table-backup.sfdisk` before starting. `sfdisk /dev/<disk> < table-backup.sfdisk` restores it.

## Command line

```
bootshift probe <disk>                    show the partitions
bootshift probe <disk> --target-template  print the layout as a target file to edit
bootshift plan  <disk> <target.json>      show the steps
bootshift check <disk> <target.json>      run every safety check
bootshift apply <disk> <target.json>      check, then prepare the change for the next restart
```

A target file maps each partition's PARTUUID to its new start and length in sectors.

## Building

```
cargo build --release --target x86_64-unknown-linux-musl   # the static bootshift binary
cargo test --workspace
cargo run -p bootshift-gui -- --demo                       # the desktop app, on a made-up demo disk
cd gui && npm ci && npm test && npm run build              # the web frontend
```

## Repository layout

| Path | Contents |
|---|---|
| `crates/core` | Pure logic: layout model, planner, validation, journal format. No I/O. |
| `crates/disk` | Block device I/O, crash-simulating test disks, GPT, journal storage |
| `crates/fs` | Filesystem adapters (ext4, NTFS, Btrfs, XFS, swap) driving the standard tools |
| `crates/boot` | GRUB redirect, boot order, journal file, recovery initramfs |
| `crates/engine` | Preflight checks, copier, executor, recovery, prepare step |
| `crates/cli` | The `bootshift` binary, used in the normal system and as the recovery system's init |
| `crates/helper` | `bootshiftd`, the polkit-checked D-Bus service the GUI talks to |
| `crates/gui` | `bootshift-gui`, the desktop app. Talks only to `bootshiftd`; shows a demo disk when the helper isn't running |
| `gui/` | The TypeScript reference frontend the desktop app was built from |
| `packaging/` | D-Bus, systemd and polkit files |

## Contributing

- [`task.md`](task.md): the ordered work plan, and the research behind the design.
- [`CLAUDE.md`](CLAUDE.md): the safety rules, code style and testing rules all contributions follow.

BootShift is licensed under the GNU GPL, version 3 or later.
