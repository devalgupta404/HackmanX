# BootShift — Task Plan

Ordered work plan for BootShift, from an empty repo to a release tested on real laptops.
The design is in `BootShift Architecture Plan.pdf` (not in the repo yet). Section 1 lists where research changed that design; when the two disagree, **this file wins**.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

There is one plan and one product: a ready-to-ship tool, run from Linux, that can move any partition anywhere on the disk. There is no reduced "v1" scope. Each task has a **Done when** line, and a task is not done until its tests exist and pass.

## 0. Product scope

| Partition contents | Move | Grow | Shrink | How |
|---|---|---|---|---|
| ext4 | yes | yes | yes | grow online after restart; shrink by `resize2fs` inside a guarded run (task 0.8) |
| NTFS | yes | yes | yes | `ntfsresize` inside a guarded run in the recovery boot; fix boot sector `0x1C` + backup boot sector |
| Btrfs | yes | yes | yes | `btrfs filesystem resize` while mounted in recovery (the kernel's relocation is transactional) |
| XFS | yes | yes | refused | `xfs_growfs`; XFS can't shrink at all |
| swap | yes | yes | yes | recreated with the same UUID and label |
| Anything else (LVM PV, LUKS, FAT data, unknown) | yes | refused | refused | moved byte-for-byte as an opaque full copy |
| EFI system partition | refused | refused | refused | holds the recovery system and journal |

**Refused, with a reason and a fix:**
- BitLocker volumes ("Turn off device encryption").
- Hibernated Windows or Fast Startup.
- Legacy BIOS or MBR disks (BootShift needs UEFI + GPT).
- systemd-boot with signed unified kernel images under Secure Boot (our initramfs can't be signed).
- USB-attached disks.
- Disks with SMART warnings.
- Laptops on battery.

**Bootloaders:** GRUB through the distro's shim (Ubuntu, Debian, Mint, Fedora, openSUSE), with Secure Boot on or off. systemd-boot without Secure Boot.

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

8. **The ext4 undo file does not protect against power loss.** The `resize2fs` and `e2undo` man pages say so explicitly, and the PDF's step 3 claim is wrong. The same is true of `ntfsresize` relocation. Every offline shrink therefore runs as a **guarded run** (task 0.8): the tool writes through a device we control, which keeps a durable undo log, so a crash rolls back to the exact pre-resize state and the tool runs again.
9. **Btrfs `device add` is not used.** It leaves data `single` across two partitions, so losing either loses the filesystem, and it complicates GRUB and the initramfs forever. Btrfs partitions are moved, then resized with `btrfs filesystem resize` while mounted in recovery.
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
18. **ESP space:** about 28 MiB is needed (measured: vmlinuz 16.2 MiB + initramfs 7.4 MiB + journal 4 MiB). That is tight on a 100 MiB Windows ESP carrying vendor firmware files.

### 1.5 Windows

19. **BitLocker is on by default on Windows 11 24H2 clean installs**, so many target laptops will be refused. A *suspended* volume is still encrypted. The fix text must say **"Turn off device encryption"** (full decrypt), and tell the user to back up the recovery key first. libblkid reports `TYPE="BitLocker"`.
20. **Windows boot after a move:** BCD finds partitions by disk GUID + partition unique GUID. We must preserve the disk GUID and every PARTUUID. Still an open risk until tested on real Windows 11 (task 0.2).
21. **WinRE after moving the recovery partition:** `ReAgent.xml` stores an offset, so it goes stale. The user runs `reagentc /disable` then `/enable` in Windows. The planner should prefer layouts that **don't move** the recovery partition.
22. **NTFS shrink happens from Linux,** with `ntfsresize` inside a guarded run, so the user never has to boot Windows to shrink it. Windows "Shrink Volume" is not used. Its limits (pagefile, hiberfil, MFT, shadow copies; Event 259 names the blocking file) only matter as advice if `ntfsresize` refuses.
23. **Remaining hole:** the user can still pick Windows from the firmware's F12 menu mid-move. Move the Windows partition first so that window is short, and show a large warning.

### 1.6 Hardware

24. **Refuse USB-attached target disks.** USB-SATA bridges commonly ignore FLUSH.
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

## 3. Decisions

- [x] **D1. Licence:** GPL-3.0-or-later. We may adapt code from kpmcore, sfdisk and GParted, keeping their copyright notices.
- [x] **D2. Shrinking:** fully supported for ext4, NTFS and Btrfs, and must be crash-safe. ext4 and NTFS shrink run as guarded runs (task 0.8).
- [x] **D3. Btrfs:** move + resize while mounted in recovery. No `btrfs device add`.
- [x] **D4. Hackathon rules:** code written before the event is allowed.
- [x] **D5. Windows recovery partition:** the planner avoids moving it when another layout reaches the target. When it must move, the summary shows the `reagentc /disable` + `/enable` step.
- [x] **D6. Scope:** one ready-to-ship product with no reduced v1. See section 0.

---

## 4. Work plan (in order)

### Phase 0 — De-risk spikes (before building on assumptions)

These test the assumptions most likely to kill the project. Use VMs, plus one real laptop where noted. Write findings back into section 1.

- [~] **0.1** Install the toolchain.
  - Rust is installed: stable 1.99.1 + `x86_64-unknown-linux-musl`. The skeleton builds and the static musl binary links.
  - Still needed (requires sudo): `qemu-system-x86 qemu-utils ovmf swtpm btrfs-progs xfsprogs smartmontools`. Then `cargo install cargo-fuzz cargo-deny`.
  - Done when: `qemu-system-x86_64 --version` works.
- [ ] **0.2** Windows boots after its partition moves.
  - In a Windows 11 eval VM (OVMF + Secure Boot), move C: offline with `sfdisk --move-data`, keeping the disk GUID and PARTUUID.
  - Fix NTFS `0x1C` and the backup boot sector, then boot.
  - Done when: Windows boots, and `chkdsk` is clean.
- [ ] **0.3** WinRE after moving the recovery partition: confirm whether it goes stale, and whether `reagentc /disable && reagentc /enable` fixes it.
  - Done when: `reagentc /info` shows Enabled at the new location.
- [~] **0.4** (one machine measured; 2–3 real laptops still to check) ESP budget.
  - Measured on 2026-10-25, kernel 7.0.0-38 (Ubuntu): vmlinuz 16.2 MiB, recovery initramfs 7.4 MiB with zstd (22 MiB raw), journal 4 MiB, about 28 MiB in total.
  - This laptop's ESP is 256 MiB with 162 MiB free.
  - Still to check: ESP free space on 2–3 real laptops, especially Windows-made 100 MiB ESPs carrying vendor files.
- [ ] **0.5** Boot redirect.
  - On Ubuntu and Fedora VMs with Secure Boot on, replace `EFI/<vendor>/grub.cfg` with a stub that boots a kernel + initrd from `EFI/bootshift/`.
  - On a systemd-boot VM with Secure Boot off, add a `loader/entries/bootshift.conf` entry and make it the default.
  - Done when: all three boot into the test initramfs and drop to a shell.
- [ ] **0.6** Guarded runs: make offline resize tools crash-safe.
  - **Goal:** run `resize2fs` (shrink) and `ntfsresize` (shrink and grow) so that a power cut at any write leaves either the untouched original or a state we can roll back to exactly. Then the tool simply runs again.
  - **Candidate A — kernel dm-snapshot.** The tool writes to a snapshot. If it succeeds and fsck passes, `snapshot-merge` folds the changes into the origin; the kernel resumes the merge after a reboot. A crash before the merge just discards the snapshot.
    - Cost: the snapshot store needs room for every block the tool writes.
  - **Candidate B — our own undo-logging device** (ublk via the `libublk` crate, or NBD). Before a block is first overwritten, its old contents are appended to an undo log and flushed. Blocks that were free in the original bitmap are not logged.
    - The log lives where the tool never writes and no data lives: free blocks beyond the new end (shrink), or the newly added space (grow).
    - Recovery replays the log backwards, then reruns the tool.
  - **Verify both** with dm-log-writes, replaying to every flush during each tool run. Measure the space they need on a 100 GB filesystem that is 60% full. Pick one and record the reasons in section 1.
  - Done when: for each tool, every replayed crash point rolls back to a state that is fsck-clean and byte-identical to the original, and the rerun then succeeds.
- [x] **0.7** Decisions D1–D6 resolved (section 3).

### Phase 1 — Repository skeleton

- [x] **1.1** Cargo workspace per the layout in `CLAUDE.md`, `LICENSE` (GPL-3.0-or-later), README, `rust-toolchain.toml`, `clippy.toml`, `.gitignore`.
  - Done when: fmt, clippy and test pass on the empty workspace, and the musl binary builds.
- [x] **1.2** Workspace lints.
  - `unsafe_code = "deny"`; only `disk` allows it, per module, with `// SAFETY:` comments.
  - `clippy::unwrap_used`, `expect_used` and `panic` are denied outside tests.
- [~] **1.3** (workflow written and passing locally; waiting for the first run on GitHub) CI (GitHub Actions): fmt, clippy `-D warnings`, `cargo test`, the musl static build, and `cargo deny` (licences + advisories).
  - Done when: a push to `main` passes CI.

### Phase 2 — Test infrastructure first

- [x] **2.1** `disk::BlockDevice` trait: `read_at`, `write_at`, `flush`, `len_bytes`, `logical_block_size`. Keep it tiny; everything that writes goes through it. In-memory disks enforce the same alignment rule as `O_DIRECT`.
- [x] **2.2** `disk::MemDisk`: a plain in-memory implementation.
- [x] **2.3** `disk::CrashDisk`: models a volatile write cache.
  - Writes stay pending until `flush`.
  - At a chosen crash point, pending writes may be dropped, applied, reordered, or torn at 512 B, and the sector in flight may be left as garbage. Without garbling, records under 512 B looked safe even in a single slot.
  - Records every write and flush, so a test can enumerate all crash points.
  - Done when: its own tests reproduce lost, reordered and torn writes deterministically from a seed.
- [ ] **2.4** Test fixtures (`cargo xtask fixture`).
  - Build disk images with a chosen GPT layout and ext4, NTFS, Btrfs, XFS and swap filesystems, filled with files of known content.
  - Write a manifest of file hashes.
  - Built with real `mkfs` + loop devices inside the test VM.
- [ ] **2.5** `cargo xtask vm-test`.
  - Boot a minimal QEMU guest (virtio-blk, `cache=none`), run the privileged integration tests as root against loop devices, and copy the results out.
  - Done when: a sample test that `mkfs.ext4`s a loop device passes in the VM and is refused on the host.

### Phase 3 — Core model and planner (pure logic, no I/O)

- [x] **3.1** `core::layout` and `core::units`: `Geometry`, `Extent`, `Partition`, `Layout`, `Target`.
  - A `Partition` holds its extent, type GUID, unique GUID, name, attributes, `Content` kind, used sectors and minimum size.
  - Sectors and LBAs are newtypes (`Lba`, `Sectors`), so mixing up bytes and sectors is a compile error. Additions saturate, so absurd input fails bounds checks instead of wrapping.
- [x] **3.2** `core::plan`: the `Operation` enum (`Shrink`, `Move`, `Grow`, `RemoveSwap`, `RecreateSwap`) and `Plan`, plus `Layout::apply`, which the executor also uses to know what the table holds after each operation.
  - NTFS boot-sector fixes are part of executing a `Move` (Phase 7), not a separate operation.
- [x] **3.3** `core::planner`: current + target layout → plan.
  - Swaps that change are removed first and recreated last. Partitions shrink in place, then move, then grow in place.
  - Moves that keep partition order never need parking and copy each partition once. Reordering parks a blocking partition in free space no target needs, or refuses if there is none.
  - Windows (NTFS) partitions move first.
  - D5 (avoid moving the recovery partition) belongs to the GUI, because the user's target decides where each partition goes.
- [x] **3.4** `core::validate`: refuses impossible or unsafe targets, each with a plain reason and a fix.
  - Missing or duplicate partitions, ESP or BitLocker changed, outside the usable area, misaligned start or end, overlap.
  - Shrinking XFS or opaque contents, growing opaque contents, shrinking below the minimum size or when it couldn't be measured.
- [x] **3.5** Property tests (`crates/core/tests/planner_properties.rs`): 10k random cases each, in about 1 s.
  - Every plan replays cleanly, never overlaps, stays on the disk, and reaches the target.
  - Order-preserving targets always plan, and copy each moved partition exactly once.
  - The direction and chunk rules belong to the copier and are tested in Phase 5.
  - Checked by mutation: breaking the planner's free-space check makes both properties fail.

### Phase 4 — Journal

- [x] **4.1** `core::journal` codec, with an explicit little-endian layout and no serde.
  - File layout: slot A at 0, slot B at 1 MiB, plan record at 2 MiB, 4 MiB in total.
  - `Slot { seq, run_id, stage, steps_done, operation, next_chunk, hash_chain }` + CRC32C, padded to 4 KiB.
  - `PlanRecord { run_id, payload }` + CRC32C. The payload's encoding is decided with the executor (Phase 8).
  - A random `run_id` per run ties slots to their plan, replacing a plan digest.
- [x] **4.2** `disk::journal_store`.
  - `JournalFile` maps file offsets onto device extents (from FIEMAP), including fragmented files.
  - `Journal::open` returns the plan and the newest valid slot for this run.
  - `Journal::commit` checks the plan record still carries this run's ID (so stale extents are never written), writes slot `seq % 2`, then flushes.
- [x] **4.3** Tests: round-trip, every single-bit flip detected, a decode test for the version-1 layout, a newer version refused, oversized plans refused, slots from another run ignored.
  - CrashDisk: every crash point × lost / landed / 64 scrambled outcomes leaves the old or the new progress; commits continue after a torn slot.
  - Mutation-checked: a single-slot journal fails the crash test.
  - Sequence wrap-around dropped: `seq` is a `u64`.
- [~] **4.4** (proptest stand-in done: `crates/core/tests/journal_properties.rs` decodes random and damaged bytes without panicking) `cargo fuzz` targets for slot and plan decoding. Needs nightly + cargo-fuzz.
  - Done when: a 30 min run finds no crashes.

### Phase 5 — Copier (the heart)

- [x] **5.1** `engine::copier`.
  - `ChunkPlan` (pure): `chunk = min(64 MiB, distance)` rounded to the logical block, the direction rule, and the used pieces of each chunk.
  - `ChunkCopier`, per chunk: read → write → flush → read back and compare → extend the hash chain. It refuses the next chunk until the last one is acknowledged as committed.
  - `move_range` commits each chunk to the journal. `verify_move` recomputes the chain over the destination.
  - The copier doesn't own the journal, so the executor can commit to a journal on a different disk from the data.
- [ ] **5.2** Read-ahead of the next chunk, overlapped with the current write (safe by the direction rule), in a small separately tested function. A performance task; correctness doesn't depend on it.
- [x] **5.3** Exhaustive crash tests (`crates/engine/tests/copier_crash.rs`).
  - Eight moves: left and right by one block, overlapping with a partial last chunk, adjacent, across a gap, chunks capped below the distance, and used ranges only.
  - Every crash point × lost / landed / 12 scrambled outcomes, then resume. Each time the destination equals the source and the hash chain verifies.
  - `copier_properties.rs`: 500 random moves with a second power cut during recovery.
  - Mutation-checked: removing the pre-commit flush, reversing the direction, chunks longer than the distance, and journaling every other chunk each fail.
- [~] **5.4** (unit tests done; dm-dust VM test pending) Read errors.
  - On EIO the copier stops, keeps the journal, and reports the exact block, found by rereading block by block.
  - `accept_unreadable` writes zeros and lists the lost blocks.
  - Unreadable blocks outside the used ranges are never read.
- [x] **5.5** Used-range copy: `CopyScope::used_only` merges ranges and always adds the first and last MiB. Pieces are block-aligned and clipped to their chunk, and chunks without data are skipped without a commit.
- [ ] **5.6** Oracle test in the VM: move a real ext4 image with our copier and with `sfdisk --move-data`, then compare the bytes.

### Phase 6 — Partition table

- [x] **6.1** `disk::GptImage`: the two GPT regions held in memory and edited with `gptman` through a sparse whole-disk view.
  - `read` falls back to the backup copy when the primary is damaged.
  - `with_partitions(layout, removable)` keeps every partition's slot, GUIDs, type, name and attributes. It refuses to drop an entry the layout doesn't describe unless it is listed as removable (a swap being recreated). Recreated entries return to their own slot.
  - `write(device)` writes only the table's own sectors: backup copy, flush, primary entries and header, flush. The protective MBR and the gap before the first partition are never written.
  - Resume needs no stored table. Rebuilding the same layout from whichever copy a crash left valid (old or new) gives the same table, so the write is simply redone.
- [x] **6.2** `disk::RealDevice` and the kernel refresh.
  - [x] The device: `O_DIRECT` through an aligned bounce buffer (no `unsafe`), `fdatasync`, BLKSSZGET via `rustix`, size from seeking to the end.
    - Writable block devices are opened `O_EXCL`, so the kernel refuses a disk with a mounted partition.
    - Tested on image files, including the copier and journal end to end (`crates/engine/tests/copier_real_device.rs`).
  - [x] `fs::kernel::refresh_partitions`: `partx --update`, which uses BLKPG per partition. BLKRRPART is never used.
- [x] **6.3** Crash tests (`crates/disk/tests/gpt.rs`).
  - Every crash point × lost / landed / 32 scrambled outcomes: the table always reads as the old or the new one. Rebuilding from the surviving copy and writing finishes with both copies holding the target and the MBR untouched.
  - Also tested: identity kept, MBR untouched, undescribed partitions refused, slot changes refused, overlap refused, swap removed and recreated in its slot.
  - Mutation-checked: dropping the flush between the copies fails the crash test.
- [x] **6.4** (`sfdisk --dump` saved to `EFI/bootshift/table-backup.sfdisk`; GUI download comes with Phase 13) User backup: `sfdisk -d` text + raw table sectors (`sfdisk --backup-pt-sectors`), saved to the ESP before the restart and offered in the GUI as a download.

### Phase 7 — Filesystem adapters (shelling out, behind one runner)

- [x] **7.1** `fs::run`: the `Runner` trait, with `SystemRunner` and `ScriptedRunner` (recorded replies, for tests).
  - Logs every command, never uses a shell, sets `LC_ALL=C` and closes stdin.
  - Stdout is kept as bytes, because `ntfscat` prints binary.
  - Closing stdin matters: `ntfsresize` treats end-of-input at its confirmation prompt as "yes". The adapters pass `--force` explicitly anyway.
- [x] **7.2** `fs::probe`.
  - `disks` runs `lsblk --json --bytes` with a fixed column list, giving disks with size, sector size, transport, removable flag and table type, and partitions with number, start, size, fstype, UUID, label, PARTUUID, type GUID and mounts.
  - `content_of` maps fstype and type GUID to `Content`. A hibernated swap (`swsuspend`) maps to opaque, so its hibernation image is only ever moved, never wiped.
  - `signature` runs `blkid --probe` for the recovery system, which has no udev.
  - `core::layout::Guid` parses the canonical text form.
  - Tests: a dual-boot + USB fixture, this machine's real `lsblk` output, and `blkid` on image files.
- [ ] **7.3** `fs::guard`: guarded runs, using the design chosen in 0.6. Durable undo log, rollback, rerun.
  - Crash tests use CrashDisk (unit) and dm-log-writes (VM).
- [~] **7.4** (adapter done; the guarded wrapping waits on 7.3) `fs::ext4`.
  - `summary` (`tune2fs -l`: state, errors, size), `check` (`e2fsck -f -p`, exit 0/1 only), `min_size` (`resize2fs -P`).
  - `used_ranges` from `dumpe2fs` free lists. Refused unless the per-group free counts add up to the superblock's.
  - `resize_offline` (to be called only inside a guarded run) and `grow_online` (run mounted in recovery, after the move).
- [~] **7.5** (adapter done; guarded wrapping waits on 7.3; boot-sector fix belongs to the executor) `fs::ntfs`.
  - `info` (`ntfsresize --info --no-action`: cluster size, volume size, minimum size). It fails on hibernated or dirty volumes.
  - `used_ranges` from `$Bitmap` (`ntfscat -i 6`). Refused if the bitmap is shorter than the volume.
  - `resize_offline` (shrink, or grow to fill the partition).
  - `DirtyMark`: `ntfsresize` always marks the volume for checking, so after BootShift's own resize the mark is expected and `--force` is passed. A volume found dirty is refused.
  - The boot sector `0x1C` and backup boot-sector fix is a raw write, done in the executor's move step.
- [ ] **7.6** `fs::btrfs`: mount in recovery; `btrfs filesystem resize <size>` before a move, `max` after. Used ranges from the dev-extent tree.
- [ ] **7.7** `fs::xfs`: move, then `xfs_growfs` online after the restart. Used ranges from `xfs_db -r` free-space data, cross-checked against a full copy.
- [x] **7.8** `fs::swap::recreate`: `mkswap --uuid --label`, tested on an image file through `blkid`. The hibernation check is the `swsuspend` signature (7.2).
- [ ] **7.9** `fs::opaque`: full byte-for-byte move with no resize, for LVM PV, LUKS, FAT data and unknown contents.
- [~] **7.10** Adapter tests against the real tools.
  - Most adapters work on plain image files, so no root or VM is needed (`crates/fs/tests/{ext4,ntfs}_tools.rs`). CI installs `e2fsprogs` and `ntfs-3g`.
  - ext4: used ranges cover every file block (`debugfs blocks`); an offline shrink keeps every file byte-identical and `e2fsck`-clean.
  - NTFS: files go in with `ntfscp`, then shrink and grow keep them byte-identical (`ntfscat`); a dirty volume is refused unless BootShift marked it.
  - End to end (`crates/engine/tests/move_real_filesystems.rs`): real ext4 and NTFS filesystems moved by the copier using only their used ranges, including power cuts mid-move and an `O_DIRECT` image-file run. They check clean and every file reads back unchanged.
    - Mutation-checked: dropping half the used ranges fails.
  - Still needs the VM: mounting (Btrfs, XFS, online ext4 grow), loop devices, and dm-dust.

### Phase 8 — Executor (the eight steps)

- [x] **8.1** `engine::executor::execute`: runs the plan from the journal alone, one step at a time, committing every step.
  - Stages: `Prepared → Running → Verifying → Finishing`. `Finishing → Done` belongs to the boot layer, which restores normal booting.
  - Per operation:
    - Shrink: guarded filesystem shrink, then the table.
    - Move: record the scope, copy, verify, fix NTFS boot sectors, then the table.
    - Grow: the table, then the filesystem.
    - RemoveSwap: the table.
    - RecreateSwap: the table, then `mkswap`.
  - Each table write rebuilds `layouts[k+1]` from whichever GPT copy is valid, then runs `partx --update`.
  - Filesystem tools sit behind the `Filesystems` trait.
  - `RunPlan` (start layout + plan) is the plan record's payload, stored as JSON so it can be read from a live USB.
  - `ntfs_boot::fix_hidden_sectors` updates both NTFS boot sectors after a move (UEFI ignores the field when the start doesn't fit 32 bits).
- [~] **8.2** (in place; real tool checks wait on the adapters' recovery wiring) Verification.
  - After each move, `verify_move` re-reads the whole destination and must match the committed hash chain before the table changes.
  - At the end, every changed filesystem goes through `Filesystems::check`.
- [ ] **8.3** dm-linear staging (from g2p/blocks): before the real table is written, map the target layout with dm-linear and fsck every moved filesystem through it. Abort before step 5 if anything fails.
- [x] **8.4** Crash tests (`crates/engine/tests/executor.rs`).
  - A real GPT disk (ESP holding the journal, plus NTFS, ext4 and swap) and a planner-made plan: remove swap, shrink, move, grow, recreate swap.
  - Every crash point × lost / landed / 4 scrambled outcomes resumes to the target table, intact data and stage Finishing.
  - Sampled second crashes during recovery resume too.
  - A finished run resumes to no work at all.
  - Mutation-checked: writing the table one operation behind fails.
- [ ] **8.5** VM integration on loop devices: the reference layout (Windows, recovery, Linux, in both directions) and random layouts, end to end. All file hashes match afterwards.
- [ ] **8.6** Cancel and undo: cancelling before step 3 is free; "Undo last change" runs the reverse plan when the space allows.

### Phase 9 — Preflight checks

- [x] **9.1** `engine::preflight::evaluate(facts, layout)`: pure; returns every refusal at once, each with a reason and a fix. Anything unknown counts as a refusal.
  - UEFI, GPT, not USB.
  - On mains with the battery ≥ 50% (desktops without a battery pass; an undetectable charger is refused).
  - SMART passed (`Unknown` refused).
  - Kernel modules present, ESP space ≥ needed (unknown refused), bootloader supported.
  - No hibernated swap, no BitLocker anywhere on the disk, no filesystem problems.
  - `engine::facts` gathers them: sysfs power (USB-C counts as mains), UEFI, `/lib/modules/<release>`, `statvfs` on the ESP, `fs::smart::health` (`smartctl --json --health`), `filesystem_problems` (`tune2fs -l` / `ntfsresize --info` on changed partitions only) and `has_hibernated_swap`.
  - The bootloader fact comes from the boot crate (Phase 10).
- [x] **9.2** Pass and refuse tests for each check: ScriptedRunner fixtures for tools, a fake `/sys` tree for power and UEFI.

### Phase 10 — Boot and recovery

- [x] **10.1** `boot::initramfs` + `boot::cpio`.
  - `CpioWriter` writes newc archives. Tested by unpacking with the system `cpio`.
  - `build_archive` copies:
    - the static `bootshift` binary as `/init`, and a `/dev/console` node;
    - the required tools (refused if any is missing) and optional Btrfs/XFS tools, each with its `ldd` libraries at the same paths;
    - module files from `modprobe --show-depends --set-version` (built-in or missing modules skipped), kmod's `modules.*` indexes, and `/etc/bootshift/modules`.
  - `compress` uses `zstd -19`.
  - Tested by building a real initramfs from this machine.
- [~] **10.2** (written; needs a VM boot to prove) Recovery as PID 1 (`cli::recover::run_as_init`, started automatically when the binary is PID 1; there is no manual `recover` subcommand).
  - Mounts proc, sys, devtmpfs, /run and efivarfs, and loads `/etc/bootshift/modules` with `modprobe`.
  - Parses `/proc/cmdline` and waits up to 60 s for the disk whose GPT disk GUID matches.
  - `engine::recovery::run` checks the table still matches the plan before the first write (if not: restore booting, finish the journal, stop). Then it executes, restores booting and commits Done. The disk is opened `ReadWriteShared` so tools can open partitions.
  - Never touches the ESP during the move; it is mounted only when restoring booting.
- [~] **10.3** (text messages done; progress bar and time left pending) Recovery screen.
  - Shows what it is doing and "If the power goes out, just turn the computer back on."
  - On an error: explains, keeps the redirect, waits for Enter, then powers off. It never boots an OS over a half-moved disk.
- [x] **10.4** `boot::journal_file` (plus `engine::prepare`, which copies the kernel and initramfs).
  - `create` writes the journal file and fsyncs it and its directory.
  - `extents` locates it with `filefrag -v -b512` (FIEMAP), refusing extents flagged delalloc, unknown, inline, encoded or not_aligned, and turning them into whole-disk offsets.
  - `boot::cmdline::RecoveryArgs` hands recovery the disk GUID and the journal's pieces on the kernel command line: `bootshift.disk=… bootshift.journal=<offset>+<len>,…`.
- [~] **10.5** (GRUB done; systemd-boot pending) `boot::grub`.
  - `recovery_config` boots `EFI/bootshift/vmlinuz` and `initrd.img` with no menu.
  - `install_redirect` saves the original once, then atomically replaces `EFI/<vendor>/grub.cfg` (temp file, fsync, rename, fsync directory). `remove_redirect` puts it back. Both are safe to repeat.
  - systemd-boot (a loader entry plus the default in `loader.conf`) is still to do.
- [x] **10.6** `boot::efi`: parses `efibootmgr`.
  - `recovery_order` puts BootCurrent (the running distro) first and drops Windows Boot Manager from the order; the entry itself is kept.
  - `set_order` uses `efibootmgr --bootorder`.
  - Saving the original order in the plan record comes with the prepare step.
- [x] **10.7** Done step (`engine::recovery::restore_booting`): restore BootOrder, mount the ESP, put `grub.cfg` back, unmount, then commit Done. Every action is safe to repeat. Files in `EFI/bootshift/` are left for the normal system to clean up.
- [x] **10.8** Escape hatch: `bootshift resume <disk> "<options>"` finishes a run from any live USB, using the options saved in `EFI/bootshift/recovery-args.txt`. It refuses a disk whose GPT GUID doesn't match.

### Phase 10b — Command line

- [x] **10b.1** `bootshift probe <disk> [--target-template]`, `bootshift plan <disk> <target.json>` and `bootshift check <disk> <target.json>`.
  - All three only read. They need root to open the disk and say so when it is missing.
  - `engine::probe::layout` builds the planner's layout from the GPT, `lsblk`, `tune2fs`/`resize2fs -P` and `ntfsresize --info`. A failed measurement counts as full with no minimum.
  - `engine::facts::gather` collects every preflight fact. GRUB vendor: the only one, or the one named like BootCurrent's entry; refused if that can't be told.
  - A target file maps PARTUUIDs to `{start, len}` in sectors. `probe --target-template` prints the current layout to start from.
- [~] **10b.2** (written; needs a VM run to prove) `bootshift apply <disk> <target.json> [--yes] [--init <static binary>]`.
  - Plans and runs every check. Without `--yes` it stops, having changed nothing.
  - Refuses an `--init` binary that isn't static.
  - `engine::prepare`, in order:
    1. Copy vmlinuz, build and copy the initramfs.
    2. Capture swap UUIDs and labels (`blkid`) and the original BootOrder.
    3. Write the journal with a random run ID, then read it back through the raw disk.
    4. Back up the table with `sfdisk --dump`, and save `recovery-args.txt`.
    5. Install the GRUB redirect, then set the boot order (the redirect is undone if this fails).
- [x] **10b.3** Recovery (PID 1) and `bootshift resume` (from a live USB): see 10.2 and 10.8.

### Phase 11 — Crash and boot test suites (VM)

- [ ] **11.1** Kill-QEMU resume suite: random layouts on qcow2 overlays, with QEMU killed at random moments, thousands of times overnight.
  - This tests resume logic only: killing QEMU doesn't drop the disk's write cache. Lost writes are covered by 11.2.
- [ ] **11.2** dm-log-writes suite: log every write of a full run. For each flush or FUA mark, `replay-log` onto a zeroed target, run recovery, and check fsck plus every file hash.
- [ ] **11.3** dm-dust suite: unreadable sectors in source regions → clean stop, journal intact, resumable after the user accepts.
- [ ] **11.4** OVMF Secure Boot suite (`OVMF_CODE.secboot.fd` + `swtpm`).
  - Windows 11 eval + Ubuntu, Windows 11 eval + Fedora, and one systemd-boot distro with Secure Boot off.
  - Run moves, grows and shrinks, then confirm every OS boots.
  - Never commit the Windows images.
- [ ] **11.5** Nightly CI job wiring up 11.1–11.4 on a self-hosted KVM runner.

### Phase 12 — Privileged helper

- [x] **12.1** `bootshiftd`: serves `org.bootshift.Helper1` at `/org/bootshift/Helper1` on the system bus (zbus).
  - Methods: `Version`, `Probe(disk)`, `Plan(disk, target)`, `Check(disk, target)`, `Apply(disk, target)`.
  - Replies are JSON: `{"result":"ok","value":…}` or `{"result":"refused","refusals":[{reason, fix}]}`. Other failures are D-Bus errors.
  - Every method asks polkit (`zbus_polkit`) first: `org.bootshift.inspect`, and `org.bootshift.apply`, which is always `auth_admin` (never `_keep`).
  - Slow work runs off the D-Bus executor with `blocking::unblock`.
  - Smoke-tested on a private bus: the name is owned, `Version` answers, and introspection shows all five methods.
- [x] **12.2** The helper never trusts a plan from the GUI. It receives only a target, re-inspects the disk, re-plans, and `service::apply` runs every check again.
- [x] **12.3** Packaging files in `packaging/`:
  - D-Bus system policy: root owns the name, anyone may call.
  - D-Bus activation file pointing at `bootshiftd.service`.
  - systemd unit (`Type=dbus`).
  - polkit policy: inspect is allowed for the active user; apply always asks for the admin password.
  - The XML is checked with `xmllint`.

### Phase 13 — GUI (Tauri 2)

- [~] **13.1** (frontend done; the Tauri shell waits on webkit2gtk/GTK dev packages) The disk bar: plain TypeScript + Vite in `gui/`.
  - "Now" and "After the restart" bars; partitions are blocks sized to the disk, with a used-space strip.
  - Drag a block to move it, drag its edges to resize; arrow keys move 1 GiB, Shift+arrows resize. Everything snaps to 1 MiB.
  - Partitions smaller than 1% of the disk are drawn above their neighbours so they stay visible. The EFI system and BitLocker partitions are locked.
  - `editing.ts` holds the pure clamping rules mirroring `core::validate` (alignment, minimum size, can grow/shrink, no overlap, stay on the disk), with 12 Vitest tests.
  - Driven in a browser with Playwright against the demo backend: a keyboard shrink, a keyboard move and a mouse edge drag each clamp correctly, and the plan list updates. No console errors.
- [~] **13.2** (frontend done) Plan preview with the same wording as the CLI and a copy estimate ("Copies about 90.5 GiB, during one restart"). Refusals and unavailable steps are shown with their fixes.
- [~] **13.3** (frontend done) Backup confirmation checkbox. Apply is enabled only when the plan has steps, nothing is unavailable, the checks passed and the box is ticked. The table-backup download is still to do.
- [ ] **13.4** Apply → restart; post-restart summary, including the `reagentc` step when the recovery partition moved and the one-time chkdsk note for NTFS.
- [ ] **13.5** Tauri 2 shell (`gui/src-tauri`): commands `list_disks`, `probe`, `plan`, `check` and `apply` forward to `org.bootshift.Helper1` over the system D-Bus (a zbus proxy). Needs `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev` to build.

### Phase 14 — Docs and packaging

- [ ] **14.1** README: what it does, what it refuses and why, and how to recover if something goes wrong.
- [ ] **14.2** `docs/design.md`: the PDF design updated with section 1.
- [ ] **14.3** Packages: `.deb`, `.rpm` and AppImage for the GUI, which pull in the filesystem tools as dependencies.
- [ ] **14.4** Demo script and recording for the hackathon.

### Phase 15 — Real hardware gate

- [ ] **15.1** 2–3 laptops from different makers.
  - At least one with a cheap SATA SSD, and one running Windows 11 24H2 with encryption turned off.
  - Run moves, grows and shrinks, with power-cord pulls at random steps.
- [ ] **15.2** Release only when:
  - every suite in Phases 5–11 is green;
  - 15.1 shows zero data loss;
  - every refusal message has been seen once by a human.

---

## 5. Test pyramid

| When | What | Time budget |
|---|---|---|
| Every commit | unit tests, proptest planner, journal round-trip, CrashDisk every-point for small plans, fmt, clippy, cargo deny | < 5 min |
| Nightly | CrashDisk randomized on large plans, dm-log-writes replay, dm-dust, kill-QEMU × thousands, fuzz 30 min, adapter VM tests | hours |
| Pre-release | OVMF Secure Boot with Windows 11 + Ubuntu/Fedora, real-laptop power pulls | manual |
