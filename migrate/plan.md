# BootShift Migrate — Plan

Move a running machine (usually a VM) to a new, often larger machine while it keeps taking writes, with seconds of data downtime, without losing a byte, and with proof that nothing was lost.

This folder holds the design for a separate service built on BootShift's engine. Read the three files in this order:

1. `plan.md` (this file): the problem, the research, what we sell, the safety rules and the ordered work plan.
2. `architecture.md`: the parts, how data flows, the state machines and what happens when anything fails.
3. `implementation.md`: crates, types, algorithms, wire and on-disk formats, and tests.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done. A task is done only when its tests exist and pass. The repository's `CLAUDE.md` rules apply here too.

This plan supersedes the earlier note `docs/machine-to-machine-migration.md`.

---

## 0. The problem

A team has a VM (or physical server) that must move to a new one:
- a larger disk;
- a newer VM generation or a different hypervisor or cloud;
- or a newer OS.

The old machine is busy. It takes writes all the time, so it can't be switched off for the hours a full copy takes.

**What we promise:**
- **Data downtime of a few seconds:** freeze, then send the last changes.
- **Total downtime of under a minute:** add the boot of the new machine.
- **The new machine is byte-for-byte what the old one held at the freeze moment**, and we can prove it.
- **The old machine is never modified**, so going back is one command.

**What we don't promise:**
- **Zero downtime.** That needs the CPU and RAM state moved too, which only a shared hypervisor can do (vMotion, QEMU live migration). Our total downtime is one boot of the new machine.
- **Database-level continuity.** For a database, application replication can be better; for example, Postgres logical replication gave Gadget about 3 s of downtime. We detect databases and say so.

---

## 1. Research summary

The research ran on 2026-10-26 in four streams: commercial products, open-source code, practitioner write-ups, and papers plus kernel primitives. Every claim below has a source in §8.

### 1.1 How the commercial products work

| Product | How writes are captured | Change model | Cutover | Weak spots |
|---|---|---|---|---|
| **AWS MGN / DRS** (CloudEndure lineage) | In-guest kernel driver; block map in about 250 MB of RAM; no journal on disk | Continuous; **full rescan** after a hard reboot, crash, agent reinstall or disk resize | Launches from the latest state. **Does not stop the source**; the user must quiesce. "Minutes" | Kernel headers must match the running kernel exactly; no Secure Boot; backlog and stalls on write-heavy servers; AWS only |
| **Azure Migrate / ASR** | Agent: InMage filter driver plus a process server. Agentless: VMware snapshot + CBT | Agent: continuous. Agentless: cycles of 1–12 h | User shuts down the source, then the final delta | Snapshots can fill the datastore; cycles reset after a 90-minute outage. The only product with a **built-in checksum per cycle** |
| **Google M2VM** (Velostrata) | Snapshot + CBT every 2 h. Velostrata v4 was the only **post-copy** design: boot in the cloud, stream blocks | Cycles | Stop the source, final sync | Post-copy reached end of support in 2022; rollback is manual |
| **Nutanix Move** | Agentless: VMware snapshot + CBT, through VDDK or DirectPath | Snapshot about every 10 min | Stop the source, final snapshot, power on in AHV | Re-seeds when stale or under heavy churn; AHV only |
| **VMware HCX** | vSphere Replication (Bulk); RAV ends with a vMotion | 2 h RPO | Bulk: one reboot | Broadcom: the offline sync for high-churn VMs can take "minutes to several hours"; vSphere only |
| **Zerto** | Hypervisor I/O splitter plus a journal at the target | Continuous, seconds | Move with commit or rollback | Licensed per VM; hypervisor-level only |

The common shape is: **copy everything once, keep sending changes, optionally test, then a short final sync and switch.** We keep this shape.

### 1.2 What the code and papers teach

1. **Mirroring writes beats chasing dirty blocks.** VMware's ESX storage-migration paper (ATC'11) measured the same Exchange workload with three designs:
   - repeated dirty-block passes: **13.3 s** of downtime;
   - incremental dirty-block passes: **7.6 s**;
   - synchronous IO mirroring: **0.22 s**.

   At double the load, both dirty-block designs **failed to finish** and mirroring still finished. A synchronous mirror slows the writer to the speed of the destination, so it always converges.
2. **Background copying can stall for ever.** A Proxmox user's migration sat at 96% for 8 hours. QEMU's own comment in `block/mirror.c` says that under heavy I/O "we will not converge anyway". Proxmox's fix is to copy in background mode, then switch to `write-blocking` mode near the end and poll for `actively-synced`.
3. **Start tracking before the bulk read, and accept duplicate copies.** Proxmox adds the dirty bitmap *before* replication starts.
   - QEMU's silent data-loss bug LP#2156307 came from a gap where neither the old nor the new tracker was recording.
   - Its corruption showed up in MySQL and PostgreSQL.
   - The lesson: tracking windows must overlap.
4. **Clear the dirty bit before reading the chunk, and set it again if anything fails.** This is QEMU's `mirror_iteration` pattern.
5. **Persistent tracking marks a block durably before the write it covers is issued, and fails closed.** dm-era, DRBD's activity log and the md write-intent bitmap all work this way.
   - **RAM-only trackers lose everything on a crash.** These include MGN's agent, blksnap, and QEMU bitmaps after an unclean shutdown, which come back `+inconsistent`.
6. **Change-tracking APIs have shipped wrong data:**
   - VMware CBT after a disk grew past 128 GB;
   - CBT again on ESXi 8.0u2;
   - XFS corruption after a Forklift warm migration (MTV-1656).

   So **verify with hashes at the end**; never trust the tracker alone.
7. **Post-copy loses atomicity.** In post-copy designs (Hines VEE'09, dm-clone, Fly.io), the new machine boots before the copy is complete and fetches blocks as it needs them. If either machine or the link fails while it is still fetching, data is split across two machines. Fly.io uses this anyway, but kernel nbd hung on network blips, so they moved to iSCSI.
8. **An intermediate copy is never a consistent image.** Only the final pass after a quiesce is. (ALICE OSDI'14; Seneca USENIX'03.)
9. **Throttling the writer is the only fix when the dirty rate is above the link rate.** Clark NSDI'05 stuns rogue processes; Bradford VEE'07 adds delays per write; XvMotion ATC'14 injects vCPU delays.
10. **Practitioners' failures are mostly not the copy:**
    - drivers missing in the initramfs;
    - stale fstab entries;
    - BIOS/UEFI mismatch;
    - changed IPs;
    - literal IPs in configs;
    - AD machine-account password rotation;
    - quotas;
    - procedure errors on cutover day.

    Rehearsals and preflight checks catch these. Gadget ran "dozens and dozens" of dry runs and turned every failure into a preflight check.
11. **Converting the replica in place is risky.** Coriolis snapshots the replica before changing it for the new hardware and restores the snapshot on error. virt-v2v makes these changes offline: virtio drivers in the initramfs, device names in fstab and GRUB, first-boot scripts.
12. **What the kernel offers for capturing writes in a guest:**
    - **eBPF `block_bio_queue`** fires once per bio, after partition remapping, on the whole-disk device. It also fires again for device-mapper's lower bios. So it sees writes from partitions, LVM, md, `O_DIRECT`, writeback and swap.
    - **Its blind spots** are passthrough commands (SG_IO, NVMe char-device `uring_cmd`).
    - **eBPF is observational.** It can't delay a write. But if the program sets the bitmap bit inside the kernel, the bit is set at submission, before the write completes.
    - **Lockdown:** integrity mode, the usual one under Secure Boot, allows plain tracepoint programs.
    - **Inserting a device-mapper layer** under an already mounted plain partition is impossible without a remount. On LVM it is routine; `pvmove` does it.

### 1.3 Gaps nobody covers (where we win)

*Verified against vendor docs, October 2026; citations in §8.*

- **No product publishes a final proof** that the target equals the source at the freeze point. Azure agentless runs an **internal** per-cycle checksum but keeps no artefact you can hold or re-verify; everywhere else integrity validation is a **manual, do-it-yourself step** (run your own hashes, or row-level DB validation via AWS DMS). None sign a receipt that anyone can re-check.
- **Failures cost a lot.** A target or link failure often means a rescan, a reseed or a cycle reset.
- **No product reshapes the target disk as part of the migration.** AWS MGN *can* enlarge a volume (you choose a bigger EBS size in the launch template), **but growing the filesystem and fixing the partition layout is a manual post-migration step**; Nutanix/HCX/Move clone the disk at its source size. None move partitions or grow filesystems automatically during the migration. (So the earlier "MGN is 1:1" shorthand was wrong: MGN enlarges the *raw volume*, not the layout.)
- **The source is always touched.** Agent tools (MGN, Zerto) install software in the source guest. Agentless tools (Azure Migrate, Nutanix Move, VMware HCX) take **VMware snapshots on the source datastore** each cycle and consolidate them, with guest stun. None read the source purely read-only.
- **Every product is locked to one target**, and the in-guest agents need kernel modules.
- **No product predicts downtime from measured churn** or gives a go/no-go gate.
- **The guest changes for new hardware are undocumented** and can't be undone.

---

## 2. What we sell

**Headline (the three verified differentiators, §1.3):** *Migrate any Linux machine onto any new disk — automatically reshaped and grown to fit — with a signed receipt proving it is byte-for-byte identical, and the source never written, so rollback is one power button.*

The three starred USPs below (**U1 proof**, **U4 reshape**, **U6 untouched source**) are the headline: no incumbent has any of them, and all three reuse BootShift's copier, journal, planner and grow adapters almost directly. Everything else — Track mode, Mirror mode, rehearsal — makes the migration **faster, not more differentiated**, so it is built *on top* of the verified base, in the §5 order, each step "only faster, never less safe". **Downtime is a measured spec, not the headline:** sub-second in LVM mirror mode, seconds with the tracker, minutes under heavy churn with no tracker — always finishing safely, because the cutover is verified-first (MD4) and fails closed (§A7).

| # | USP | Who lacks it | How we deliver it |
|---|---|---|---|
| **U1 ★** | **Proof of copy.** A signed receipt with the blake3 Merkle root of the source at freeze, which must equal the root of the target, re-verifiable by anyone with both disks. After go-live, a background re-read of the preserved source checks every byte with no extra downtime | **All.** Azure's per-cycle checksum is internal and kept by no one; elsewhere integrity validation is a manual DIY step (your own hashes, or AWS DMS row checks) | Receiver keeps a durable hash per chunk (§A4 in `architecture.md`); source re-read after cutover |
| U2 | **Resume, never restart.** A target or link failure resumes from the last chunk the receiver recorded. A source reboot needs only a hash-compare pass that re-reads the disk but sends only the differences | MGN rescans; Azure resets after 90 min; Move reseeds | Journal plus hash index on the receiver; the sender marks every unacknowledged range dirty again |
| U3 | **Cutover that is guaranteed to converge, with a forecast.** We measure churn and link speed, predict the final delta and downtime, and step up from tracking to a throttle to a synchronous mirror. Cutover is a go/no-go gate on the predicted downtime | Proxmox stalls; MGN backlog; HCX "minutes to hours" | Convergence controller (§A6) |
| **U4 ★** | **Target reshaped to its new disk, automatically.** Moved partitions, grown filesystems, extended LVM, identity preserved — done during the migration | MGN enlarges the *raw volume* but you grow the filesystem and fix the layout by hand afterward; Nutanix/HCX/Move clone at source size. None reshape the layout | BootShift planner + grow adapters |
| U5 | **Any to any, no kernel module, Secure Boot OK.** One static binary; eBPF CO-RE, so no per-kernel build; LVM/dm built in. Works between KVM, VMware, Hyper-V, any cloud and bare metal | MGN (headers, no Secure Boot); Move/HCX (one hypervisor) | In-guest agent; target boots our receiver image |
| **U6 ★** | **Untouched source, one-step rollback.** The source's data disks are read **read-only**; the source is never written (no agent, no snapshot). Cutover is verified-first and fails closed. Rollback is leaving the source running | Agent tools install software in the guest; **agentless tools snapshot the source datastore each cycle (delta writes + consolidation + stun).** None read-only | Read-only source + verified-first cutover (§A7) |
| U7 | **Rehearse on real data while replication continues**, including an OS-upgrade dry run, and measure the cutover | Only MGN tests, and only in AWS | Writable dm-snapshot of the replica exported to a throwaway VM |
| U8 | **Guest changes you can see and undo.** Every change for new hardware is recorded in an undo log and shown as a diff | All vendors make undocumented edits | BootShift `guard` / `undo_log` |
| U9 | **Database-aware.** Detect database engines; run app freeze hooks; recommend logical replication when it fits better | — | Preflight + hooks |

---

## 3. Safety rules (migration invariants)

These sit alongside the BootShift invariants in `CLAUDE.md`. Code that breaks one is wrong even if the tests pass.

- **M1. The source is read-only.** The migrator never writes the source's data disks. The one exception is LVM mirror mode (§A5.3), which adds a leg to a volume group, and only with explicit consent shown in the summary.
- **M2. Refuse before starting.** Preflight on both machines passes before the first byte moves. A check that can't decide is a refusal.
- **M3. Flush, verify, then record, then acknowledge.** The receiver acknowledges a chunk only after its data is `fdatasync`ed, read back with `O_DIRECT` and matched against the sender's hash, and after its hash-index entry and journal record are durable.
- **M4. Tracking fails closed.** A bit is set when the write is submitted *and* when it completes. Any gap in tracking marks the whole disk dirty, so the next pass is a full hash compare. Gaps include:
  - the program being detached;
  - a counter showing lost events;
  - an unknown writer (passthrough);
  - the agent restarting;
  - a source reboot.

  "Clean" is never inferred without either an unbroken tracker or a hash comparison.
- **M5. Start tracking before reading.** The tracker is live and confirmed before the first chunk of any pass is read. Overlapping windows are mandatory, after QEMU LP#2156307.
- **M6. The final pass runs only on a quiesced, drained source.** All filesystems are frozen, swap is off, and no write was seen after the freeze took effect. A write seen after the freeze aborts the cutover and thaws the source.
- **M7. One live copy, fail-closed.** A fencing epoch is in both journals. **The target boots only with *both* its own durable `Committed` *and* the source's `SourceSurrendered(epoch)` token**, emitted after the source freezes and powers off or is fenced; `Committed` alone never boots, for that epoch, for ever. The source never thaws itself into a live state after it has surrendered; *before* surrender its watchdog may thaw it back to serving, which is safe because the target cannot have booted without the token. Rollback is explicit and records `RolledBack` before the source resumes, but it is a convenience, not the safety mechanism. A link partition during the final pass therefore cannot produce two live copies (the earlier "boots on `Committed`" wording could).
- **M8. The target boots only after the final verify passes and the receipt is durable.** All guest changes go through the undo log.
- **M9. Preserve identity:** disk GUID, PARTUUIDs, filesystem UUIDs, LVM UUIDs. Exceptions are changes the user chose (new partition sizes), and each is recorded in the receipt.
- **M10. No post-copy in any default mode.** Booting before the copy is complete gives up failure atomicity. It may only ever be added as a separately named opt-in mode with its own invariants.
- **M11. Never TRIM the source.** Discards seen by the tracker count as writes.

---

## 4. Decisions to confirm with the team

- [ ] **MD1. Where the code lives:** `migrate/crates/*` as members of the root workspace (proposed), or a separate repository.
- [ ] **MD2. Writing eBPF in Rust:** use `aya-ebpf` for the BPF program (proposed). The BPF crate needs `unsafe` and a nightly build via `cargo xtask`, which is an exception to "only `disk` may use unsafe". The alternative is C with libbpf, which needs clang.
- [ ] **MD3. First sources:** Linux guests (ext4, XFS, Btrfs, LVM). Windows guests (VSS freeze plus a minifilter-free tracker) come later.
- [x] **MD4. Default cutover: verified-first.** The final compare pass proves the target *before* it boots, so we never serve unverified data — this is the only choice consistent with BootShift's one rule (never lose or corrupt data). In v1 (Compare mode) the final pass *is* the proof, so verified-first costs no extra work. "Fast" (prove in the background after go-live) is an explicit opt-in for low-churn, low-risk jobs. (Reversed from the earlier "fast" proposal: defaulting to detect-after-the-fact can serve corrupted data in the window between cutover and the background verify, which contradicts the one rule.)
- [ ] **MD5. Default transport:** TLS 1.3 over TCP with a 6-word pairing code (proposed). Alternative: SSH tunnel.

---

## 5. Work plan (in order)

Each task has a **Done when** line.

### Build order — foundation first, then accelerators

We build the whole system. The order is about safety, not scope: each stage is correct on its own, and every later stage only makes the **same** migration faster, never less safe (§I12).

1. **Foundation — Compare mode (Phases 1–5, 6.2, 7, 8, 11, 12).** A correct, verified-first, agentless migration that reshapes onto the new disk and signs a receipt: the three starred USPs, with no tracker to break and the source read-only. Everything after is an accelerator on this base. Depends only on spikes 0.4, 0.5, 0.8.
2. **Track mode (0.1–0.3, 6.1).** Cuts a low-churn migration from minutes to seconds of data downtime.
3. **Mirror mode (0.6, 9).** Sub-second convergence for high-churn LVM machines — the one mode that writes the source (the M1 exception, with explicit consent).
4. **Rehearsal (0.7, 10) and throttle (6.4).**

A stage's Phase-0 spikes gate it (e.g. Track needs 0.1–0.3). If a spike can't be met on a given host, that host simply runs the stage below it — the foundation always works. Build the test infrastructure (Phase 2) before the feature it tests, per `CLAUDE.md`.

### Phase 0 — De-risk spikes

- [ ] **0.1 eBPF tracker sees every write.** Attach a BPF program to `block_bio_queue` and to the completion tracepoint in a test VM on Ubuntu 22.04 (5.15) and 24.04 (6.8). Drive these write paths with fio:
  - buffered and `O_DIRECT`;
  - LVM, dm-crypt, md raid1;
  - swap;
  - discard and write-zeroes.

  Record `blktrace` as ground truth.
  **Done when** the bitmap is a superset of the blktrace writes in 100 runs, and passthrough writes (sg_io, `/dev/ng*`) are detected by the driver tracepoints and reported as "tracking broken".
- [ ] **0.2 The clear-and-drain protocol has no lost bit.** Model the "set, re-check generation, set again" protocol (§A5.1) in a userspace simulation that tries every interleaving of a writer with a drain; a loom test is acceptable.
  **Done when** no interleaving loses a bit.
- [ ] **0.3 eBPF under Secure Boot lockdown.** Load the tracker on Ubuntu with Secure Boot on (integrity lockdown).
  **Done when** it loads and runs, or the refusal message is written.
- [ ] **0.4 Freezing `/` safely.**
  - The agent is resident: `mlockall`, no file I/O after the freeze.
  - It freezes every filesystem, swap is off, and a watchdog thaws after N seconds; `SysRq-j` is documented as a manual escape.
  - Under fio load, no write may be seen after `FIFREEZE` returns on all filesystems.

  **Done when** 1,000 freeze/thaw cycles under load show no hang, and every hang injected in the test is undone by the watchdog.
- [ ] **0.5 The receiver boots the replica.**
  - **Secure Boot on:** the receiver `kexec_file_load`s the replica's distro-signed kernel.
  - **Secure Boot off:** plain kexec.
  - **Fallback when kexec is refused:** set the boot entry and reboot.

  **Done when** a replicated Ubuntu boots on KVM and on one cloud, by kexec and by the reboot fallback.
- [ ] **0.6 LVM mirror over the network.** Export the target's PV partition over NVMe/TCP and over iSCSI (LIO). `lvconvert --type raid1 -m1` on the source; measure write latency; pull the network for 5 s and for 5 min; restore it.
  **Done when** the source never stalls more than the configured timeout, the raid resyncs only changed regions, and the target-side VG can be cleaned up (missing PV removed) as one idempotent step.
- [ ] **0.7 Rehearsal snapshot.** Take a dm-snapshot (writable) of the replica while the receiver keeps writing to the origin; export it to a throwaway VM; boot it and run `do-release-upgrade`.
  **Done when** the origin's hash index still matches after the rehearsal, and the COW store's fill rate is measured.
- [ ] **0.8 Throughput.** Measure read, zstd, TLS and write at 1 and 10 GbE with 4 KiB to 4 MiB extents.
  **Done when** the bulk copy reaches ≥ 80% of line rate on 10 GbE with LZ4/zstd-1.

### Phase 1 — Skeleton

- [x] **1.1** Create the crates of `implementation.md` §I1, with workspace lints. Also add the eBPF build to `xtask` (`cargo xtask build-tracker`).
  **Done when** `cargo build --workspace` and clippy pass, and `bootshift-migrate --help` runs.

### Phase 2 — Test infrastructure first

- [x] **2.1 `LiveDisk`.** A `MemDisk` that a seeded writer thread mutates concurrently while recording a ground-truth write log.
  **Done when** a seed reproduces the exact same history.
- [x] **2.2 `SimTracker`.** A tracker fed from `LiveDisk`'s log, with injectable faults: drop, delay, detach.
- [x] **2.3 `FaultyLink`.** An in-memory transport that can drop, delay, duplicate or cut frames, and reconnect.
- [x] **2.4 Two-sided crash harness.** Receiver on `CrashDisk`: crash at every write or flush point × resume × link cut at every frame.
  **Done when** the harness runs a 64 MiB migration exhaustively in under 2 min.
- [x] **2.5 Two-VM QEMU rig** in `xtask` (`cargo xtask migrate-vm-test`): a source VM running fio and Postgres, a target VM booted into the receiver, a switch, and kill-QEMU on either side.

### Phase 3 — Pure logic (`migrate-core`)

- [x] **3.1 Wire messages and codec.** proptest round-trip plus a fuzz target.
- [x] **3.2 Receipt format.** Canonical JSON, a blake3 Merkle root over chunk hashes, ed25519 signature. Includes a decode test and a fuzz target.
- [x] **3.3 Dirty map model.** Range set at 4 KiB granularity, merged into extents of at most 4 MiB.
- [x] **3.4 Convergence model.** Predict the final delta and downtime from the history of each pass, and decide among continue, throttle, mirror, ready, and can't-converge.
  **Done when** proptest shows it never says *ready* while predicted downtime is above budget, and never loops for ever on a fixed churn rate.
- [x] **3.5 Cutover state machine.** Pure states on both sides, plus the fencing epoch.
  **Done when** exhaustive tests over every crash point on both sides find no state where two copies are writable, or none is recoverable.
- [x] **3.6 Target layout plan.** Map source partitions to target extents on a bigger disk (reuses the `bootshift-core` planner).
  **Done when** proptest shows every source byte maps to exactly one target byte and identity fields are preserved.

### Phase 4 — Receiver

- [x] **4.1 Journal and hash-index store** on the scratch device (format in `implementation.md` §I4). Includes the decode-old-version test rule.
- [x] **4.2 Applier.** Batch write → `fdatasync` → read back with `O_DIRECT` → compare hashes → index entries → flush → journal → acknowledge.
  **Done when** the CrashDisk test covers every write and flush point with dropped, reordered and torn pending writes, and the final target equals the source at freeze.
- [ ] **4.3 Index service.** Stream the hash index to the sender for compare passes.

### Phase 5 — Sender

- [ ] **5.1 Source probe and preflight** (§A3), refusals with reason and fix.
- [x] **5.2 Bulk reader.** Used ranges only, via the `bootshift-fs` adapters; always the first and last MiB of each partition. Reads are chunked and pipelined.
- [x] **5.3 Compare pass.** Hash source chunks, compare with the index, send mismatches.
- [x] **5.4 Resend on reconnect.** Every unacknowledged range is marked dirty again.

### Phase 6 — Tracking and convergence

- [x] **6.1 eBPF tracker backend** (§A5.1), with drop, detach and passthrough detection and generation-flip draining.
- [x] **6.2 Compare-only backend** (fallback when nothing else is available).
- [x] **6.3 Pass loop** driven by the convergence model.
- [ ] **6.4 Throttle step.** cgroup v2 `io.max` on the busiest writers, only with consent and with a configured ceiling.
- [ ] **6.5 Downtime forecast** in the CLI and GUI: "ready: predicted data downtime 2.1 s".

### Phase 7 — Cutover

- [x] **7.1 Quiesce.**
  - Run app hooks.
  - Swap off.
  - `FIFREEZE` every filesystem, deepest mount first.
  - Confirm no write after the freeze; the watchdog thaws if this doesn't finish in time.
- [x] **7.2 Final pass, final verify, receipt, `Committed` on both sides.**
- [ ] **7.3 Rollback:** a recorded `RolledBack` on the target, then a thaw on the source.

### Phase 8 — Target finalize

- [x] **8.1 Lay out the target disk** from the plan: GPT, partitions at new places, backup header at the new end.
- [x] **8.2 Grow:**
  - the filesystem: ext4 offline in the receiver; XFS and Btrfs mounted in the receiver;
  - LVM: `pvresize`, `lvextend`.
- [x] **8.3 Adapt for the new hardware through the undo log:**
  - storage and network drivers in the initramfs;
  - fstab entries and GRUB device names;
  - network config by MAC;
  - serial console.

  Show the diff.
- [ ] **8.4 Boot:** `kexec_file_load`, with a firmware-reboot fallback, and a boot health check.

### Phase 9 — LVM mirror mode

- [x] **9.1 Receiver exports the PV partition** (NVMe/TCP, then iSCSI) with a fencing token in the export name.
- [x] **9.2 Sender controls the mirror:**
  - attach the leg, `lvconvert raid1`;
  - watch the sync;
  - on a link outage, let the leg degrade and continue (the source never blocks);
  - on reconnect, resync only the changed regions.
- [x] **9.3 Cutover in mirror mode:** freeze → sync at 100% → `Committed` → the target removes the missing PV in one idempotent journaled step.
- [ ] **9.4 Non-LVM parts** (ESP, `/boot`) go through the tracker path in the same run.

### Phase 10 — Rehearsal

- [x] **10.1 `rehearse`.** Pause the applier at a pass boundary, take a dm-snapshot, export it, resume the applier.
  **Done when** a throwaway VM boots the rehearsal copy and the hash index is unchanged.
- [ ] **10.2 Rehearsal report:**
  - boot time;
  - service health checks;
  - measured cutover timings;
  - guest-change diff.

### Phase 11 — Proof after go-live

- [ ] **11.1 Background source verify.**
  - Read the frozen or powered-off source (in the BootShift recovery image, or by attaching its volume) and compare with the receipt.
  - On mismatch, report the exact chunks and offer a rollback.

  **Done when** a deliberately corrupted target chunk is detected and named.

### Phase 12 — Interfaces

- [ ] **12.1 CLI:**
  - `bootshift-migrate receive`;
  - `send <pairing-code>`;
  - `status`, `rehearse`, `cutover`, `rollback`, `verify`, `finalize`.
- [ ] **12.2 `bootshiftd` D-Bus methods** for the sender side, plus GUI screens built from the core types: churn chart, forecast, cutover button, receipt.

### Phase 13 — Test suites

- [ ] **13.1 Two-VM suite:**
  - fio at increasing write rates;
  - Postgres pgbench with a consistency check after cutover;
  - kill either VM at random points (1,000 runs);
  - network partition at every protocol state.
- [ ] **13.2 Fault injection:**
  - dm-dust read errors on the source;
  - full target disk;
  - full scratch disk;
  - full COW store during a rehearsal.
- [ ] **13.3 Downtime benchmark** reported for each release: data downtime and total downtime at 4 churn levels.

### Phase 14 — Real-world gate

- [ ] **14.1** KVM → KVM, VMware → KVM, AWS EC2 → another cloud, bare metal → VM. Each with a rehearsal, a cutover, a background verify, and a rollback drill.

---

## 6. Acceptance targets

| Metric | Target |
|---|---|
| Data downtime (freeze to final acknowledgement), 1 GbE, churn below link rate | ≤ 5 s |
| Total downtime (freeze to services healthy on target) | ≤ 60 s on KVM with kexec |
| Bytes lost or corrupted | 0, proved by the receipt plus the background verify |
| Restart after a target crash or link cut | Resumes from the last acknowledged chunk; no data re-sent before it |
| Guest overhead during tracking | < 3% throughput on fio randwrite (measure in Phase 0) |
| Mirror mode, extra write latency | ≈ one network round trip + target flush; shown in the forecast |

---

## 7. Non-goals for the first release

- Moving CPU and RAM state (live migration of a running kernel). Use a hypervisor for that.
- Post-copy, "boot first, fetch later" (rule M10).
- Windows sources (planned: VSS freeze + tracker).
- Shrinking filesystems on the target. Grow and move only, at first; shrink later through BootShift guarded runs.
- Upgrading the OS automatically. We make it safe to rehearse and to undo; the upgrade itself is the user's tool (`do-release-upgrade`).

---

## 8. Sources

**Commercial:**
- AWS MGN [Agent FAQ](https://docs.aws.amazon.com/mgn/latest/ug/Agent-Related-FAQ.html), [cutover](https://docs.aws.amazon.com/mgn/latest/ug/launch-cutover-gs.html), [launch FAQ](https://docs.aws.amazon.com/mgn/latest/ug/AWS-Related-FAQ.html), [replication lag](https://repost.aws/knowledge-center/mgn-linux-fix-replication-lag), [agent install](https://repost.aws/knowledge-center/mgn-linux-fix-replication-agent-install), [DRS replication FAQ](https://docs.aws.amazon.com/drs/latest/userguide/Replication-Related-FAQ.html), [DRS API states](https://docs.aws.amazon.com/drs/latest/APIReference/API_DataReplicationInfo.html)
- Azure [agent-based architecture](https://learn.microsoft.com/en-us/azure/migrate/agent-based-migration-architecture), [agentless concepts](https://learn.microsoft.com/en-us/azure/migrate/concepts-vmware-agentless-migration), [prepare for migration](https://learn.microsoft.com/en-us/azure/migrate/prepare-for-migration)
- Google [M2VM lifecycle](https://docs.cloud.google.com/migrate/virtual-machines/docs/5.0/discover/lifecycle), [VM adaptations](https://docs.cloud.google.com/migrate/virtual-machines/docs/5.0/resources/vm-adaptations), [Velostrata whitepaper](https://cloud.google.com/files/understanding-the-velostrata-technology-for-mass-migrations-into-google-cloud-platform.pdf)
- Nutanix [Move DirectPath](https://www.nutanix.com/blog/flexible-vm-migrations-to-the-nutanix-cloud-platform-with-move-directpath), [solution brief](https://www.nutanix.com/content/dam/nutanix/en/resources/solution-briefs/sb-move-application-migration.pdf)
- VMware HCX [Bulk Migration KB 323663](https://knowledge.broadcom.com/external/article/323663/hcx-bulk-migration-operations-and-best.html)
- Zerto [Move process](https://help.zerto.com/bundle/Admin.ZSSP.HTML.10.0_U3/page/The_Move_Process.htm)

**Code:**
- QEMU [`block/mirror.c`](https://gitlab.com/qemu-project/qemu/-/blob/d73149283205645625bfa9fe029cfb68e4c4a9ae/block/mirror.c), [LP#2156307](https://bugs.launchpad.net/ubuntu/+source/qemu/+bug/2156307)
- libvirt [`qemu_migration.c`](https://gitlab.com/libvirt/libvirt/-/blob/742c1555b7d439ca8edca6297093c74e2c20a913/src/qemu/qemu_migration.c)
- Proxmox [`QemuMigrate.pm`](https://git.proxmox.com/?p=qemu-server.git;a=blob;f=src/PVE/QemuMigrate.pm;hb=71ea0ac7192bb935f63cb64beb59e87d6cecd3bf), [active-mode patch](https://lists.proxmox.com/pipermail/pve-devel/2024-May/063785.html)
- Forklift [`migration.go`](https://github.com/kubev2v/forklift/blob/ed0fa80edcdac9a099f50f905b7e3aef2a70b798/pkg/controller/plan/migration.go), [MTV-1656](https://issues.redhat.com/browse/MTV-1656)
- virt-v2v [`convert_linux.ml`](https://github.com/libguestfs/virt-v2v/blob/cdb2da63f32fbba1d7ed8f74afef4ea7f361f7b5/convert/convert_linux.ml)
- [Coriolis](https://github.com/cloudbase/coriolis/tree/3b1b879cffa2e1da2adbbeac922508aca1c08e7f)
- DRBD [`drbd_actlog.c`](https://github.com/LINBIT/drbd/blob/cdd14f16bac15bbe3f56607e0b60e88f52b24da2/drbd/drbd_actlog.c)
- [dm-era](https://github.com/torvalds/linux/blob/master/drivers/md/dm-era-target.c), [dm-clone](https://docs.kernel.org/admin-guide/device-mapper/dm-clone.html)
- [blksnap](https://github.com/veeam/blksnap/blob/7e05e6f85230964ec2fe94ae1d70f491a91cd5d5/doc/blksnap.md), [dattobd](https://github.com/datto/dattobd/blob/90cde6e468052fadbf4dabe5bd3cafc0a2fe700f/src/cow_manager.c)
- [xmigrate](https://github.com/xmigrate/xmigrate/blob/29a378c7279e711d9b7b09ca7af607d7b1bfd0e9/ansible/payloads/start_dd_aws.py)

**Papers:**
- Clark et al. [NSDI'05](https://www.usenix.org/legacy/event/nsdi05/tech/full_papers/clark/clark.pdf)
- Bradford et al. VEE'07 ([abstract](https://www.cl.cam.ac.uk/Research/SRG/netos/seminars/0607/abstracts/07may10.html))
- Luo et al. [CLUSTER'08](https://pages.mtu.edu/~zlwang/papers/cluster08.pdf)
- Hines & Gopalan [VEE'09](https://kartikgopalan.github.io/publications/hines09postcopy.pdf)
- Mashtizadeh et al. [ATC'11](https://www.usenix.org/legacy/event/atc11/tech/final_files/Mashtizadeh.pdf), [XvMotion ATC'14](https://www.usenix.org/system/files/conference/atc14/atc14-paper-mashtizadeh.pdf)
- Zheng et al. [VEE'11](https://www.cs.rice.edu/~eugeneng/papers/VEE11.pdf)
- [Seneca USENIX'03](https://static.usenix.org/events/usenix03/tech/full_papers/ji/ji.pdf)
- Zhao et al. [APSys'24](https://www.eecg.utoronto.ca/~zhaojiec/papers/smartnic-livemigration-apsys24)

**Practitioners:**
- Fly.io [Making Machines Move](https://fly.io/blog/machine-migrations/) ([HN](https://news.ycombinator.com/item?id=41109292))
- Proxmox ["Migration never end"](https://forum.proxmox.com/threads/migration-never-end.100766/)
- Google [lessons from live migration](https://cloud.google.com/blog/products/gcp/lessons-learned-from-a-year-of-using-live-migration-in-production-on-google-cloud/)
- dev.to [Hidden Realities of Cloud Migration](https://dev.to/sauveer_ketan/the-hidden-realities-of-cloud-migration-lessons-from-the-trenches-54cb)
- Gadget [zero-downtime Postgres upgrades](https://gadget.dev/blog/zero-downtime-postgres-upgrades-using-logical-replication)
- PagerTree [Postgres migration](https://pagertree.com/blog/postgres-migration-minimal-downtime)
- [blkhash (FOSDEM'23)](https://fosdem.org/2023/schedule/event/vai_blkhash_fast_disk/)
- [XReplicator](https://news.ycombinator.com/item?id=47527328)

**Kernel:**
- [fsfreeze(8)](https://man7.org/linux/man-pages/man8/fsfreeze.8.html), [pvmove(8)](https://man7.org/linux/man-pages/man8/pvmove.8.html)
- [qemu-ga](https://www.qemu.org/docs/master/interop/qemu-ga-ref.html)
- [NBD protocol](https://github.com/NetworkBlockDevice/nbd/blob/master/doc/proto.md)
- [blkfilter on LWN](https://lwn.net/Articles/934493/)
