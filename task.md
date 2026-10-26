# BootShift — Task Plan

Ordered work plan for BootShift, from an empty repo to a release tested on real laptops.
The design is in `BootShift Architecture Plan.pdf` (not in the repo yet). Section 1 lists where research changed that design; when the two disagree, **this file wins**.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done · **MVP** = needed for the hackathon demo path
(Windows NTFS + recovery partition + Linux ext4, giving space from Windows to Linux).

Each task has a **Done when** line. A task is not done until its tests exist and pass.

---

## 1. Research findings that change the design

These are the results of the research pass on 2026-10-25. Each item is a correction or addition to the PDF.

### 1.1 Data movement

1. **Journal/write ordering (corruption bug in the PDF).** When `2 × chunk > distance`, chunk *i+1*'s destination overlaps chunk *i*'s source. If chunk *i+1* reaches disk before "chunk *i* done" is durable, a resume re-copies chunk *i* from overwritten data.
   **Rule:** write chunk → `fdatasync` → commit journal → `fdatasync` → only then issue any write for the next chunk. Read-ahead is always safe; write-ahead is not.
2. **Source checksums can't be re-read after the move.** Once a source region is overwritten it is gone. Per chunk: read source → hash in memory → write → `fdatasync` → read back destination with `O_DIRECT` and compare → commit. The journal keeps a constant-size **hash chain** (`H_i = blake3(H_{i-1} ‖ hash(chunk_i))`), and the final verify recomputes the chain over the destination.
3. **Copy-only-used must not parse raw bitmaps.** ext4 groups flagged `BLOCK_UNINIT` have no on-disk bitmap, and flex_bg moves bitmaps around. Use e2fsprogs (`dumpe2fs` "Free blocks:" lines) instead of our own parser. For NTFS, `$Bitmap` (`ntfscat -i 6`) doesn't cover the backup boot sector in the partition's last sector. **Always copy the first and last MiB of every partition**, whatever the bitmap says.
4. **An NTFS move does change bytes inside the filesystem.** Boot sector offset `0x1C` ("hidden sectors") holds the partition start LBA, and the backup boot sector must be at the partition's new last sector. Fix both after a move (kpmcore and partclone do this).
5. **Full copy first, used-only second.** The copier must work and pass every crash test copying the whole partition. Used-range copying is an optimization layered on top and cross-checked against the full copy.

### 1.2 Journal

6. **Don't write the journal through vfat.** Every vfat write updates directory entries under `EFI/`, so a torn write can damage boot files.
   - Before the restart, preallocate a fixed-size journal file (e.g. 4 MiB) on the ESP, `fsync` it and its directory, then read its physical extents with `FIEMAP`.
   - The recovery system writes those extents **raw** (`O_DIRECT` + `fdatasync`) after checking a magic value. FAT metadata is never touched after planning.
   - The ESP stays **unmounted** during the move. This also satisfies kernels built with `CONFIG_BLK_DEV_WRITE_MOUNTED=n`.
7. **Journal slot layout:** two slots, each in its own 4 KiB-aligned block, ≥ 1 MiB apart. Each slot holds magic, version, sequence number, length and CRC32C. Never share a 4 KiB block between slots (512e drives turn a 512 B write into a 4 KiB read-modify-write).

### 1.3 Filesystems

8. **The ext4 undo file does not protect against power loss.** The `resize2fs` and `e2undo` man pages say so explicitly, and the PDF's step 3 claim is wrong. ext4 **shrink** is not resumable. Keep it off the MVP path. See decision D2.
9. **Btrfs "link natively" (`btrfs device add`) should be opt-in, not the default.** It leaves data `single` across two partitions, so losing either loses the filesystem. It also complicates GRUB and the initramfs forever. Default for Btrfs: move the partition, then `btrfs filesystem resize max` online.
10. **fsck can't run in the normal system.** `e2fsck -f -n` on a mounted fs gives false errors. Pre-checks only read state (`tune2fs -l`: clean, no `needs_recovery`, error count 0, and `ntfsresize --check --no-action`). Run the real `e2fsck -f -p` in the recovery boot, abort on exit code ≥ 4.
11. **Swap:** refuse if the header ends with `S1SUSPEND` (a hibernation image is present). Recreate with `mkswap -U <uuid> -L <label>`.
12. **ntfsresize always leaves the volume dirty**, so Windows runs chkdsk once. Tell the user before Apply.

### 1.4 Boot path

13. **We can't copy signed GRUB into our own folder.** Its config path is compiled in (`/EFI/ubuntu`, `/EFI/fedora`).
    - Instead, temporarily replace the distro's ESP stub `EFI/<vendor>/grub.cfg` with a BootShift config that boots `EFI/bootshift/vmlinuz` + `initrd.img`. Keep the original as `grub.cfg.bootshift-orig`.
    - Every boot path that reaches the distro shim then lands in recovery: the NVRAM entry, a BootOrder reset, or the `fbx64.efi` fallback.
14. **BootNext alone is unsafe.** It is consumed on the first boot, so after a power cut firmware follows BootOrder again. Set a persistent BootOrder with the distro first and Windows Boot Manager **removed from the order** (the entry itself is kept). Restore both in step 8.
15. **Secure Boot works as designed.** shim_lock verifies the kernel, not the initrd, so an unsigned initramfs is fine. Lockdown mode doesn't block block-device writes, `O_DIRECT`, BLKPG or efivars. Never ship our own shim, GRUB or kernel modules.
16. **Refresh the kernel's partition view with BLKPG per-partition ioctls, not BLKRRPART** (it returns EBUSY while any partition is open).
17. **Initramfs:** build our own cpio from the running system's binaries, libraries and modules. Required modules: `nvme nvme_core vmd ahci sd_mod vfat fat nls_cp437 nls_iso8859_1 efivarfs xhci_pci usbhid hid_generic i8042 atkbd` (+ `virtio_blk virtio_pci` for VMs). Modules are often `.ko.zst`, so ship `kmod`. **Refuse to plan if `/lib/modules/$(uname -r)` is missing** (kernel updated without a reboot).
18. **ESP space:** about 25–35 MB is needed (estimated: vmlinuz 14–17 MB plus a zstd initramfs 8–15 MB). Tight on a 100 MB Windows ESP with vendor firmware files. Measure in task 0.4.

### 1.5 Windows

19. **BitLocker is on by default on Windows 11 24H2 clean installs**, so many target laptops will be refused. A *suspended* volume is still encrypted. The fix text must say **"Turn off device encryption"** (full decrypt), and tell the user to back up the recovery key first. libblkid reports `TYPE="BitLocker"`.
20. **Windows boot after a move:** BCD finds partitions by disk GUID + partition unique GUID. We must preserve the disk GUID and every PARTUUID. Still an open risk until tested on real Windows 11 (task 0.2).
21. **WinRE after moving the recovery partition:** `ReAgent.xml` stores an offset, so it goes stale. The user runs `reagentc /disable` then `/enable` in Windows. The planner should prefer layouts that **don't move** the recovery partition.
22. **Windows Shrink Volume limits:** the pagefile, hiberfil, MFT and shadow copies block it. The guided step covers `powercfg /h off`, temporarily disabling the pagefile and System Restore, a reboot, then shrink. Event 259 names the blocking file.
23. **Remaining hole:** the user can still pick Windows from the firmware's F12 menu mid-move. Move the Windows partition first so that window is short, and show a large warning.

### 1.6 Hardware

24. **Refuse USB-attached target disks in v1.** USB-SATA bridges commonly ignore FLUSH.
25. **Never issue TRIM/discard during a move.**
26. **A flush is not a full guarantee.** In a FAST'13 study, 13 of 15 SSDs corrupted data under power faults, even after a flush. The final verify and the backup prompt are essential, not optional.

---

## 2. Prior art and what we reuse

| Project | What it gives us | Licence | Verdict |
|---|---|---|---|
| util-linux `sfdisk --move-data` ([sfdisk.c](https://github.com/util-linux/util-linux/blob/master/disk-utils/sfdisk.c) `move_partition_data`) | Overlapping move, direction logic; writes table first, no resume | GPL-2.0+ | Idea only; **test oracle** for the copier |
| GParted `Copy_Blocks.cc` | Rollback copies back only the overwritten region (in memory only) | GPL-2.0+ | Idea only |
| KDE kpmcore [`externalcommandhelper.cpp`](https://invent.kde.org/system/kpmcore/-/blob/master/src/util/externalcommandhelper.cpp) | Root D-Bus helper with a polkit check per method; NTFS `0x1C` fix | GPL-3.0+ | Idea only; **model for our helper** |
| [g2p/blocks](https://github.com/g2p/blocks) | device-mapper staging: build target layout as dm-linear, fsck it, then commit | GPL-3.0 | Idea only; **use in tests** |
| e2fsprogs (`e2fsck resize2fs dumpe2fs tune2fs e2image`) | ext4 check, resize, used blocks | GPL-2 | Shell out |
| ntfs-3g (`ntfsresize ntfscat ntfsfix`) | NTFS check, grow, `$Bitmap` | GPL-2 | Shell out |
| btrfs-progs | `btrfs filesystem resize`, dev-extent tree | GPL-2 | Shell out |
| util-linux (`lsblk -J -b -O`, `blkid -p`, `sfdisk -d`, `mkswap`) | Enumeration, BitLocker detection, table backup | GPL-2 | Shell out |
| smartmontools (`smartctl -j -H -A`) | Disk health incl. NVMe | GPL-2 | Shell out |
| efibootmgr | BootOrder edits | GPL-2 | Shell out |
| kmod | Load `.ko.zst` modules in recovery | LGPL-2.1 | Ship in initramfs |
| udisks2 | Read-only device list and change signals | LGPL | GUI only, optional; never for writes |

**Rust crates** (all MIT/Apache/CC0 — compatible with any licence choice):

| Need | Crate |
|---|---|
| GPT read/write | `gptman` (call `GPTHeader::write_into` per copy with a flush between; its `GPT::write_into` doesn't flush) |
| ioctls, `O_DIRECT`, `fdatasync` | `rustix` (+ `nix` ioctl macros for BLKGETSIZE64, BLKPG) |
| Chunk hashes / journal CRC | `blake3` / `crc32c` |
| D-Bus + polkit | `zbus`, `zbus_polkit` |
| JSON from lsblk/blkid/smartctl | `serde`, `serde_json` |
| Errors / CLI | `thiserror` (libraries), `anyhow` (binaries), `clap` |
| GUI | `tauri` 2 |
| Testing | `proptest`, `proptest-state-machine`, `cargo-fuzz` + `arbitrary`, `tempfile` |

**Licence note:** running GPL tools as separate processes places no obligation on our code. Copying code from sfdisk, GParted, kpmcore or blocks would make BootShift GPL. The initramfs is built on the user's machine from their distro packages, so we never redistribute GPL binaries.

**Research papers and what each one tells us:**

- **ALICE** (OSDI'14): never assume writes land in order or all at once. CRC and sequence-number every record.
- **CrashMonkey/B3** (OSDI'18): crash bugs show up within a few operations after a flush. Test every crash point around each flush exhaustively.
- **Torturing Databases** (OSDI'14): replaying a block trace up to every write finds real bugs. That is what our dm-log-writes tests do.
- **SSD power fault** (FAST'13): flushed data can still be corrupted. Verify, and prompt for a backup.
- **FiSC/EXPLODE** (OSDI'04/'06): test reordered and dropped unflushed writes, not just log prefixes. That is what our `CrashDisk` models.
- **Tools:** [dm-log-writes](https://docs.kernel.org/admin-guide/device-mapper/log-writes.html) + [`replay-log`](https://github.com/josefbacik/log-writes) (used by xfstests generic/482), and [dm-dust](https://docs.kernel.org/admin-guide/device-mapper/dm-dust.html) for unreadable sectors.

---

## 3. Decisions to make before coding

- [ ] **D1. Licence.** Recommended: **GPL-3.0-or-later**. It lets us adapt kpmcore and sfdisk code directly, and matches the disk-tool ecosystem. Alternative: MIT/Apache-2.0 (Rust norm), taking ideas only.
- [ ] **D2. ext4 shrink in v1.** Recommended: allowed only behind an explicit "this step can't survive a power cut — confirm you have a backup" consent. On resume after a crash mid-shrink, run `e2fsck -f -y` and stop with a report. Not on the MVP path.
- [ ] **D3. Btrfs.** Recommended: move + online grow by default; `device add` hidden behind an advanced option or deferred to v2.
- [ ] **D4. Hackathon rules on pre-written code.** If not allowed, use the time before the event for spikes (Phase 0) and test fixtures only.
- [ ] **D5. Moving the Windows recovery partition.** Recommended: the planner avoids it when another layout exists. When it must move, show the `reagentc` step in the summary.

---

## 4. Work plan (in order)

### Phase 0 — De-risk spikes (manual, before building on assumptions)

These test the assumptions most likely to kill the project. Use VMs plus one real laptop. Write findings back into section 1.

- [ ] **0.1** Install toolchain: `rustup` (stable + `x86_64-unknown-linux-musl`), `qemu-system-x86`, `ovmf`, `swtpm`, `btrfs-progs`, `smartmontools`, `cargo-fuzz`. **MVP**
  Done when: `cargo --version` and `qemu-system-x86_64 --version` work.
- [ ] **0.2** Windows boots after its partition moves. In a Windows 11 eval VM (OVMF + Secure Boot), move the C: partition offline with `sfdisk --move-data`, keeping the disk GUID and PARTUUID, and fix NTFS `0x1C` plus the backup boot sector. Then boot. **MVP**
  Done when: Windows boots, and `chkdsk` is clean.
- [ ] **0.3** WinRE after moving the recovery partition: confirm whether it goes stale and whether `reagentc /disable && reagentc /enable` fixes it.
  Done when: `reagentc /info` shows Enabled at the new location.
- [ ] **0.4** ESP budget: build a test initramfs (kernel modules from 1.4 + e2fsprogs + ntfs-3g + kmod) and measure vmlinuz + initramfs. Check ESP free space on 2–3 real laptops. **MVP**
  Done when: the numbers are recorded in 1.4, item 18.
- [ ] **0.5** Secure Boot redirect: on Ubuntu and Fedora VMs with Secure Boot on, replace `EFI/<vendor>/grub.cfg` with a stub that boots a kernel + initrd from `EFI/bootshift/`. **MVP**
  Done when: both boot into the test initramfs and drop to a shell.
- [ ] **0.6** `ntfsresize` grow under power cut: grow NTFS in a VM, kill QEMU mid-run 20 times.
  Done when: after each run, either the grow completes on retry or the volume is unchanged.
- [ ] **0.7** Resolve D1–D5.

### Phase 1 — Repository skeleton

- [~] **1.1** (skeleton committed; not yet built, licence pending D1) Cargo workspace per the layout in `CLAUDE.md`: `crates/core`, `crates/disk`, `crates/fs`, `crates/boot`, `crates/engine`, `crates/cli` (`bootshift`), `crates/helper` (`bootshiftd`), `gui/`, `xtask/`. Licence file, README stub, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`. **MVP**
- [ ] **1.2** Workspace lints: `unsafe_code = "deny"` everywhere except `disk`; `clippy::unwrap_used`, `expect_used`, `panic` denied in non-test code of `core`, `disk`, `engine`. **MVP**
- [ ] **1.3** CI (GitHub Actions): fmt, clippy `-D warnings`, `cargo test`, musl static build of `bootshift`, `cargo deny` (licences + advisories). **MVP**
  Done when: an empty PR passes CI.

### Phase 2 — Test infrastructure first

- [ ] **2.1** `disk::BlockDevice` trait: `read_at`, `write_at`, `flush`, `len`, `logical_block_size`. Keep it tiny; everything that writes goes through it. **MVP**
- [ ] **2.2** `disk::MemDisk`: plain in-memory implementation. **MVP**
- [ ] **2.3** `disk::CrashDisk`: models a volatile write cache. Writes are pending until `flush`. At a chosen crash point, pending writes may be dropped, applied, reordered, or torn at 512 B. It records every write and flush so a test can enumerate all crash points. **MVP**
  Done when: its own tests show it reproduces lost, reordered and torn writes deterministically from a seed.
- [ ] **2.4** Test fixtures (`xtask fixture`): build disk images with a chosen GPT layout, ext4/NTFS/Btrfs/swap filesystems filled with files of known content, and a manifest of file hashes. Built with real `mkfs` + loop devices inside the test VM. **MVP**
- [ ] **2.5** `xtask vm-test`: boot a minimal QEMU guest (virtio-blk, `cache=none`), run the privileged integration tests (`#[ignore]`-gated, `--include-ignored`) as root against loop devices, and copy results out. Destructive tests never run on the host. **MVP**
  Done when: a sample test that `mkfs.ext4`s a loop device passes in the VM and is refused on the host.

### Phase 3 — Core model and planner (pure logic, no I/O)

- [ ] **3.1** `core::layout`: `Disk`, `Partition` (start/end in sectors, type GUID, unique GUID, name, attrs, `Filesystem` kind, used bytes), `Layout`. Units are sectors (`u64`) wrapped in newtypes (`Lba`, `Sectors`) so byte/sector mixups are compile errors. **MVP**
- [ ] **3.2** `core::plan`: the `Operation` enum (`ShrinkFs`, `MovePartition`, `SetPartitionBounds`, `GrowFs`, `RecreateSwap`, `FixNtfsBootSector`) and `Plan` (ordered operations, bytes-to-copy estimate). **MVP**
- [ ] **3.3** `core::planner`: current + target layout → plan. Strategy ladder: grow in place → recreate swap → move used blocks (Btrfs `device add` only if D3 allows). Choose the sequence that copies the fewest bytes. Never move the ESP, prefer not moving the recovery partition (D5), order operations so the Windows partition moves first. **MVP**
- [ ] **3.4** `core::validate`: refuse unsupported targets (ESP moved, overlap, outside disk, misaligned, shrink below used + margin, XFS shrink, …), each with a user-facing reason and fix. **MVP**
- [ ] **3.5** Property tests (proptest): for random layouts and targets, every plan reaches the target, no two partitions ever overlap at any intermediate step, every move respects the direction and chunk rules, and the bytes copied are ≤ the naive plan. Plus `proptest-state-machine` for apply/undo symmetry. **MVP**
  Done when: 10k cases pass in < 30 s.

### Phase 4 — Journal

- [ ] **4.1** `core::journal` codec: `Slot { magic, version, seq, step, op_index, next_chunk, hash_chain, plan_digest, crc32c }` plus the full plan blob. Encode/decode with explicit little-endian layout; no serde for on-disk format. **MVP**
- [ ] **4.2** `disk::journal_store`: A/B slots at fixed offsets inside the preallocated extent list; `commit()` writes the older slot, then flushes; `load()` returns the newest slot with a valid CRC. **MVP**
- [ ] **4.3** Tests: round-trip, every single-bit flip is detected, torn slot write falls back to the other slot (CrashDisk), seq wrap-around. **MVP**
- [ ] **4.4** `cargo fuzz` target for `Slot::decode` and plan decode. Done when: 30 min run, no crashes.

### Phase 5 — Copier (the heart)

- [ ] **5.1** `engine::copier::move_range(dev, src, dst, len, ranges, journal)`: direction rule, `chunk = min(64 MiB, distance)` aligned to the physical block size, strict per-chunk order from 1.1 item 1, read-back verify, hash chain. Resumes from `next_chunk`. Copies full range when `ranges` is "all". **MVP**
- [ ] **5.2** Read-ahead of the next chunk overlapped with the current write (safe per the direction rule). Keep it behind a simple, separately tested function.
- [ ] **5.3** Exhaustive crash tests on CrashDisk: for small moves (left and right, distance < chunk, = chunk, ≫ chunk, overlapping and not), crash at **every** recorded point with every drop/reorder/tear option, then resume. **MVP**
  Done when: the final bytes equal the expected layout for 100% of crash points.
- [ ] **5.4** Read error path: on EIO, stop, keep the journal, report the exact LBA. Only continue past it with an explicit `--accept-unreadable` choice. Test with a `FaultyDisk` wrapper (unit) and dm-dust (VM). **MVP**
- [ ] **5.5** Used-range copy: input is a sorted list of ranges, plus first and last MiB always. Tests compare against the full copy for random bitmaps. **MVP**
- [ ] **5.6** Oracle test in VM: move a real ext4 image with our copier and with `sfdisk --move-data`, then compare bytes. **MVP**

### Phase 6 — Partition table

- [ ] **6.1** `disk::gpt`: read with `gptman`; write the **backup** entries and header, flush, then the primary entries and header, flush. Preserve disk GUID, PARTUUIDs, type GUIDs, names, attributes. **MVP**
- [ ] **6.2** Kernel refresh via BLKPG per partition (del/add/resize), never BLKRRPART while anything is open. **MVP**
- [ ] **6.3** Crash tests: crash anywhere in 6.1. Resume writes the journal's target table again, and the result is always the target table. **MVP**
- [ ] **6.4** User backup: `sfdisk -d` text + raw table sectors (`sfdisk --backup-pt-sectors`), saved to the ESP before restart and offered in the GUI as a download. **MVP**

### Phase 7 — Filesystem adapters (shelling out, behind one runner)

- [ ] **7.1** `fs::run`: one wrapper around `std::process::Command` that logs the command line, captures stdout/stderr, maps exit codes to typed errors, and never invokes a shell. **MVP**
- [ ] **7.2** `fs::probe`: parse `lsblk -J -b -O` and `blkid -p -o export` → `core::layout`. Detects BitLocker, LUKS, LVM, RAID, XFS, USB transport. Unit tests with captured JSON fixtures from real machines. **MVP**
- [ ] **7.3** `fs::ext4`: read-only state check (`tune2fs -l`), `e2fsck -f -p` (recovery only), min size (`resize2fs -P` + margin), used ranges (`dumpe2fs` free-block lines → complement), shrink (D2), online grow. **MVP**
- [ ] **7.4** `fs::ntfs`: `ntfsresize --check --no-action`, hibernation/Fast Startup detection (`hiberfil.sys` magic + ntfsresize refusal), used ranges (`ntfscat -i 6`), grow (`ntfsresize -f`), boot sector `0x1C` + backup boot sector fix. **MVP**
- [ ] **7.5** `fs::swap`: hibernation-signature check, recreate with same UUID/label.
- [ ] **7.6** `fs::btrfs`: move + `btrfs filesystem resize max`, used ranges from the dev-extent tree.
- [ ] **7.7** VM tests per adapter against fixture images: used ranges ⊇ what `e2image`/`ntfsclone` would copy; grow/shrink then fsck clean; all fixture file hashes intact. **MVP**

### Phase 8 — Executor (the eight steps)

- [ ] **8.1** `engine::executor`: drive `Planned → Checks → Shrink → Move → UpdateTable → Grow → Verify → Done` from the journal. Steps 4–5 repeat per partition. Every step is idempotent and resumes from the journal alone. **MVP**
- [ ] **8.2** `engine::verify`: recompute hash chains over destinations, `e2fsck -f -n` / `ntfsresize --check`, compare with the plan. **MVP**
- [ ] **8.3** dm-linear staging (from g2p/blocks): before writing the real table, map the target layout with dm-linear and fsck every moved filesystem through it. Abort before step 5 if anything fails. **MVP**
- [ ] **8.4** Crash tests: run full plans on CrashDisk with an in-memory fake for filesystem tools, crash at every journal commit and every flush, and resume until Done. **MVP**
- [ ] **8.5** VM integration: the MVP layout end to end on loop devices; all file hashes match afterwards. **MVP**
- [ ] **8.6** Undo: reverse plan after Done (only when space allows); cancel before step 3 is free.

### Phase 9 — Preflight checks

- [ ] **9.1** `engine::preflight`: each check returns `Pass` or `Refuse { reason, fix }`. **MVP**
  - SMART healthy
  - on AC power with battery > 50%
  - target disk is not USB
  - GPT + UEFI
  - no BitLocker, LUKS, LVM or RAID
  - no hibernation or Fast Startup
  - filesystems clean (read-only checks)
  - ESP has enough free space
  - running kernel's modules are present
  - shrink sizes leave a margin over used space
- [ ] **9.2** Each check has a unit test for pass and refuse, using captured tool output. **MVP**

### Phase 10 — Boot and recovery

- [ ] **10.1** `boot::initramfs`: build a cpio + zstd with the `bootshift` static binary as `/init`, plus the fs tools with their `ldd` libraries, `kmod`, and modules resolved by `modprobe --show-depends`. **MVP**
- [ ] **10.2** `bootshift recover` (PID 1 mode): mount proc/sys/dev, load modules, find the journal from the kernel command line (`bootshift.journal=PARTUUID=…:extents`), run the executor. Never mount moved partitions or the ESP during the move. **MVP**
- [ ] **10.3** Recovery text screen: current step, progress bar, time left, "If the power goes out, just turn the computer back on." On a fatal error: explain, keep the redirect in place, power off. Never auto-boot an OS over a half-moved disk. **MVP**
- [ ] **10.4** `boot::esp`: preallocate the journal file and capture its extents with FIEMAP. Copy the kernel and initramfs. Atomically swap `EFI/<vendor>/grub.cfg`: write a temp file, fsync, rename, fsync the directory. **MVP**
- [ ] **10.5** `boot::efi`: through `efibootmgr`, put the distro entry first and remove Windows Boot Manager from BootOrder, saving the original order in the journal. **MVP**
- [ ] **10.6** Done step: mount the ESP, restore `grub.cfg` and the BootOrder, delete temp files, unmount, reboot. Every action is idempotent. **MVP**
- [ ] **10.7** Escape hatch: `bootshift resume --journal …` works from any live USB with the same binary.

### Phase 11 — Crash and boot test suites (VM)

- [ ] **11.1** Kill-QEMU resume suite: run random layouts on qcow2 overlays (`qemu-img create -b base`) and kill QEMU at random moments, thousands of times overnight. This tests resume logic only; killing QEMU does not drop the disk's write cache, so lost writes are covered by 11.2. **MVP**
- [ ] **11.2** dm-log-writes suite: log every write of a full run, then for each flush/FUA mark, `replay-log` onto a zeroed target, run recovery, and check fsck plus every file hash. **MVP**
- [ ] **11.3** dm-dust suite: unreadable sectors in source regions → clean stop, journal intact, resumable after the user accepts.
- [ ] **11.4** OVMF Secure Boot suite (`OVMF_CODE.secboot.fd` + `swtpm`): Windows 11 eval + Ubuntu, and Windows 11 eval + Fedora. Run the MVP flow, then confirm both OSes boot. Never commit the Windows images. **MVP**
- [ ] **11.5** Nightly CI job wiring 11.1–11.4 on a self-hosted KVM runner.

### Phase 12 — Privileged helper

- [ ] **12.1** `bootshiftd`: zbus service `org.bootshift.Helper1` started by D-Bus activation. Methods: `Probe`, `Plan`, `Check`, `Apply(plan)`, `Status`. Polkit check (`zbus_polkit`) at the start of every mutating method, using `auth_admin` (not `_keep`). **MVP**
- [ ] **12.2** The helper re-validates the plan itself; it never trusts the GUI's plan blindly. **MVP**
- [ ] **12.3** Packaging files: D-Bus system policy, systemd unit, polkit `.policy`. **MVP**

### Phase 13 — GUI (Tauri 2)

- [ ] **13.1** Disk bar view: partitions labelled with name, size and free space. Drag edges to build the target layout, snapped to 1 MiB. **MVP**
- [ ] **13.2** Plan preview ("Copies 61 GB, about 4 minutes, one restart"), and preflight results each with its fix text. **MVP**
- [ ] **13.3** Guided Windows shrink step (from 1.5, item 22). Backup prompt, and partition-table backup download. **MVP**
- [ ] **13.4** Apply → restart. Post-restart summary, including the `reagentc` instruction when the recovery partition moved. **MVP**

### Phase 14 — Docs and release prep

- [ ] **14.1** README: what it does, what it refuses and why, how to recover if something goes wrong.
- [ ] **14.2** `docs/design.md`: the PDF updated with section 1.
- [ ] **14.3** Contributor guide (points to `CLAUDE.md` conventions).
- [ ] **14.4** Demo script and recording for the hackathon. **MVP**

### Phase 15 — Real hardware gate

- [ ] **15.1** 2–3 laptops from different makers (at least one with a cheap SATA SSD, one Windows 11 24H2 with encryption turned off). Run the MVP flow with power cord pulls at random steps. **MVP**
- [ ] **15.2** Release only when: every suite in Phases 5–11 is green, 15.1 shows zero data loss, and every refusal message has been seen once by a human.

---

## 5. Test pyramid

| When | What | Time budget |
|---|---|---|
| Every commit | unit tests, proptest planner, journal round-trip, CrashDisk every-point for small plans, fmt, clippy, cargo deny | < 5 min |
| Nightly | CrashDisk randomized on large plans, dm-log-writes replay, dm-dust, kill-QEMU × thousands, fuzz 30 min, adapter VM tests | hours |
| Pre-release | OVMF Secure Boot with Windows 11 + Ubuntu/Fedora, real-laptop power pulls | manual |
