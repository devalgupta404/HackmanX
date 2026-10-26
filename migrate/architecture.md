# BootShift Migrate — Architecture

How the service is put together, how data moves, and what happens when any part fails. Read `plan.md` first for the problem, the research and the safety rules (M1–M11) referred to here.

---

## A1. The parts

```
 SOURCE (old VM, running, busy)                          TARGET (new VM, larger disk)
 ─────────────────────────────                           ─────────────────────────────
 bootshift-migrate send   (static musl binary, root)     boots the BootShift receiver image
 ┌───────────────────────────────────────┐               ┌───────────────────────────────────────┐
 │ Probe + preflight                     │               │ Preflight (disk, firmware, space)      │
 │ Tracker  ── eBPF │ LVM mirror │ none   │               │ Receiver                               │
 │ Reader   (used ranges, chunked)       │   TLS 1.3     │  ├ Applier: write→flush→read-back→     │
 │ Pass loop + convergence controller    │ ════════════► │  │          verify→index→journal→ack   │
 │ Quiesce (hooks, swapoff, FIFREEZE)    │ ◄════════════ │  ├ Hash index + Merkle root            │
 │ Sender journal (fencing epoch only)   │    acks,      │  ├ Journal (slots, intent, epoch)      │
 └───────────────────────────────────────┘    index      │  ├ Finalize: layout, grow, adapt       │
            ▲                                            │  └ Boot: kexec / firmware reboot       │
            │ D-Bus (optional)                           └───────────────────────────────────────┘
       bootshiftd ── GUI                                   target disk (data)   scratch disk (state)
```

**One binary, two roles.** As in BootShift itself, the same `bootshift-migrate` binary runs as `send` on the source and as `receive` inside the receiver image on the target. The pure logic (protocol, convergence model, state machines, layout plan) is shared and tested once.

**Who decides what:**
- **The receiver holds the truth about the target.** That covers what is written, verified and durable.
- **The sender holds the truth about the source.** That covers what changed since it was read.
- **Neither infers the other's state.** They exchange explicit acknowledgements and, after a gap, hash indexes.

---

## A2. Deployment shapes

| Shape | How the target runs the receiver | Notes |
|---|---|---|
| KVM/Proxmox/libvirt → same | Attach the receiver ISO (or boot the BootShift recovery image from a small disk) on the new VM | Fastest; kexec avoids firmware POST |
| Hypervisor A → B (VMware → KVM, Hyper-V → KVM) | Receiver ISO on the new VM | Guest changes for the new hardware (A8.3) do the driver work |
| Cloud → cloud, on-prem → cloud | Receiver as a bootable cloud image, booted from a small boot disk; the data disk is attached as a second volume | Scratch state lives on the boot disk |
| Bare metal → VM or bare metal | Receiver ISO or PXE | Same as BootShift's recovery boot |

**Disks on the target:**
- **Data disk:** the new, larger disk the machine will run from.
- **Scratch device:** holds the receiver journal, hash index, intent log and receipt. Preferably a separate small disk (≥ 1 GiB per TiB of data). If none is available, a reserved region at the end of the data disk that the layout plan leaves unused until finalize; it is released when finalize grows the last partition.

The scratch device is written raw with `O_DIRECT` + `fdatasync`, never through a filesystem (BootShift invariant 9's reasoning).

---

## A3. Preflight

All checks run before the first byte moves (M2). Each refusal carries a reason and a fix.

**Source:**
- Kernel ≥ 5.8 with BTF (`/sys/kernel/btf/vmlinux`) for the eBPF tracker; otherwise the compare-only mode is offered, with its longer forecast.
- Every source disk is identified, with its size, logical block size and partition table. Unknown writers are refused: raw block consumers (databases on raw devices), active `nvme` char-device or SG_IO users, open dm devices we can't map.
- Filesystems are freezable (ext4, XFS, Btrfs, vfat). Swap can be turned off: enough RAM free, or the user agrees to the risk.
- Not hibernated; no BitLocker or other encryption we can't see through. LUKS is fine: it is copied byte for byte.
- Free RAM for the dirty bitmap (1 bit per 4 KiB: 32 MiB per TiB) and the read pipeline.
- Databases detected (Postgres, MySQL/MariaDB, MongoDB, Redis by process and data directory). This produces advice, not a refusal: "app hooks recommended; logical replication may give less downtime."
- Clock sanity. The convergence model uses monotonic time only, but receipts carry wall time.

**Target (receiver):**
- Data disk ≥ the source's used size plus metadata. If it is smaller than the source disk, the layout plan must fit it (shrink is a non-goal, so the plan refuses).
- Firmware type matches what the source boots (UEFI vs BIOS). A mismatch is refused unless the plan includes a bootloader conversion (later).
- Scratch device present, at least the required size, and not the data disk unless the end-region mode was chosen.
- SMART or virtual-disk health; no USB bridge (BootShift rule).
- The network link's measured round-trip time and throughput feed the forecast.

---

## A4. Receiver

### A4.1 State on the scratch device

```
 offset      contents
 0           journal slot A        (4 KiB, CRC32C, seq, run_id, epoch, stage, acked_seq)
 1 MiB       journal slot B
 2 MiB       plan record           (layout map, source geometry, written once)
 3 MiB       intent log            (ring of 4 KiB records: target ranges about to be written)
 16 MiB      hash index            (one 32-byte blake3 per 1 MiB chunk of the target data disk)
 end − 1 MiB receipt               (written at Committed, then again at Verified)
```

Slots alternate as in `bootshift-core::journal`. A torn write can only damage the slot being written. Formats and sizes are in `implementation.md` §I4.

### A4.2 The applier: the receiver's one write path

For each batch of incoming chunk frames, in stream order:

1. **Intent:** write an intent record listing the target ranges of the batch, then `fdatasync`. (This is DRBD's activity-log pattern.)
2. **Write** each chunk's data to its target offset, mapped through the layout plan.
3. **`fdatasync`** the data disk.
4. **Read back** each written range with `O_DIRECT`; its blake3 must equal the hash the sender computed over the bytes it read. On mismatch, rewrite once; if it fails again, report an error and stop. Never acknowledge it.
5. **Re-hash** every 1 MiB index chunk the batch touched (local read of the full chunk), write the index entries, then `fdatasync` the scratch device.
6. **Commit** the journal slot with `acked_seq = last frame of batch`, then `fdatasync`.
7. **Send** `Ack(acked_seq)`.

**Resume:**
- The receiver opens the newest valid slot and replays nothing.
- Every range named in an intent record newer than the committed slot is **suspect**: its index entries are marked invalid and recomputed after the sender resends.
- The sender resends from `acked_seq + 1` because it marked all unacknowledged ranges dirty again (A5).

**Ordering inside the data disk:** later passes overwrite earlier ones at the same offset. Frames are applied strictly in sequence order over one TCP stream, so the last write wins. Writes in a batch never overlap; the sender merges duplicates before sending.

Unlike BootShift's in-place move, the target never overlaps the source, so batching is safe. BootShift's invariant 4 (one unrecorded write at a time) exists only because a move's destination can overlap its own source. Here the only rule is: acknowledge after everything is durable.

### A4.3 Hash index and Merkle root

- Chunk size is 1 MiB, so 1 TiB needs 1 Mi entries × 32 B = 32 MiB of index.
- The Merkle root is blake3 over the ordered entries (a binary tree with 1 MiB leaves), cheap to recompute.
- The sender can ask for the index (or a sub-tree) to run a **compare pass**: it hashes its own chunks and sends only the ones that differ.
- At cutover, the root over the source's chunks (computed by the sender after the freeze from its own final reads, plus the index for untouched chunks) must equal the receiver's root over the target. Both roots go in the receipt.

The receiver's index entries come from read-back of durable bytes, so they describe the target disk itself, not the network stream. This is the lesson of blkhash and libguestfs: hash what was written, not what was sent.

---

## A5. Capturing changes on the source

Three backends sit behind one interface: "give me the ranges that may have changed since generation G, or tell me tracking broke." The mode is chosen at preflight:

| Source situation | Mode | Converges? | Data downtime |
|---|---|---|---|
| Root and data on LVM, LAN between machines | **Mirror** (A5.3) for LVs, **Track** for the rest | Always (synchronous) | Sub-second freeze + boot |
| Anything else with kernel ≥ 5.8 + BTF | **Track** (A5.1) | When churn < link rate; otherwise throttle (A6) | Seconds |
| Old kernel, no BTF, or eBPF refused | **Compare** (A5.2) | Each pass re-reads the disk | Time to read the hot set during the freeze; minutes |

### A5.1 Track mode: the eBPF dirty map

**Program.** The program is attached to two tracepoints:
- `block:block_bio_queue` (submission);
- the request-completion tracepoint `block:block_rq_complete`. It also covers bio-based drivers through `block:block_bio_complete` where available; Phase 0.1 settles which.

For every event on a tracked whole-disk `dev_t` whose op changes data, it sets the bits for the 4 KiB blocks covered. Ops that change data are WRITE, WRITE_ZEROES, DISCARD and SECURE_ERASE (M11).
- `block_bio_queue` fires after partition remap and again for device-mapper's lower bios, so offsets are absolute on the physical disk.
- Marking at completion as well closes the race where a write is submitted before the sender clears a bit and completes after the sender's read.

**Bitmap.** The bitmap is two BPF array maps of `u64` words (buffers 0 and 1) plus a control map holding the generation `g`. Each event runs:

```
set_bits(buf[g & 1], range)        // atomic OR (BPF_ATOMIC_OR, kernel ≥ 5.12; fall back to
g2 = control.generation            //  per-CPU maps merged in userspace on older kernels)
if g2 != g: set_bits(buf[g2 & 1], range)
```

The sender drains a generation like this:
1. Increment `g`.
2. Read and zero every word of the old buffer.
3. Treat those bits as dirty.

A program that read the old generation either finished before the flip or sees the new generation on its re-check and sets the bit in the new buffer too. A bit can be duplicated but never lost. Phase 0.2 proves this by exhaustive interleaving.

**Health counters (M4).** Any of these means **tracking broke**: the whole disk is marked dirty and the next pass becomes a compare pass.
- `events_seen`, `ranges_marked`.
- `foreign_writes`: writes to the tracked disk from passthrough paths, seen through the `nvme:nvme_setup_cmd` and `scsi:scsi_dispatch_cmd_start` write opcodes.
- `heartbeat`: the sender issues one known 4 KiB `O_DIRECT` write to a **reserved sentinel sector on the tracked disk** — a raw block write, not a file, because the filesystem is frozen during the final drain check — each pass and checks it shows up.
- Program detach, map-update errors, or an agent restart.

**Ordering with reads (M5).** At the start of every pass:
1. Confirm the tracker is attached and the heartbeat of the previous generation was seen.
2. Flip the generation.
3. Read.

The first pass flips before the first bulk read, so every write from then on is in a generation the sender will drain.

**Cost:** one map update per write bio, in kernel. The guest overhead target (< 3%) is measured in Phase 0.1.

### A5.2 Compare mode

No tracker. Each pass:
1. Read every used chunk on the source.
2. Hash it with blake3 and compare it with the receiver's index.
3. Send only the chunks that differ.

The network carries only differences, but every pass costs one full read of the used data. After the freeze, the final compare pass is exact. This is also the recovery path whenever Track mode breaks, and it is how a rebooted source rejoins (U2).

### A5.3 Mirror mode for LVM

LVM can already turn a running LV into a raid1 with a new leg, by suspending, reloading and resuming the dm table. `pvmove` relies on the same mechanism. Using the receiver's disk as that leg gives VMware's IO-mirroring design, using only tools already in the guest.

1. The receiver lays out the target disk from the plan (A8.1) and **exports the PV partition** over NVMe/TCP (Linux `nvmet`), with iSCSI (LIO) as a fallback. The export name carries the fencing epoch.
2. The sender attaches it (`nvme-tcp` or `iscsi_tcp` from the stock distro kernel). It runs `pvcreate` with the target layout's PV UUID policy and `vgextend`, then `lvconvert --type raid1 -m1 --regionsize <r>` on each LV. This is the M1 exception and needs explicit consent.
3. dm-raid copies existing data in the background. New writes go to both legs synchronously, so writes on the source now pay about one network round trip plus the target flush. The forecast shows this before consent.
4. **Link outage:** dm-raid marks the remote leg failed and the source continues on its own leg (it never blocks beyond the configured timeout; Phase 0.6). When the link returns, `lvchange --refresh` plus the write-intent bitmap resyncs only the changed regions.
5. **At cutover**, freeze, then wait for `sync_percent = 100` and in-sync health on every LV. Commit (A7). The target then runs one journaled step that removes the missing source PV from the VG and turns each LV into a plain linear LV on its own PV.
6. **After cutover**, the source still sees the remote leg in its VG metadata. Rollback detaches the leg cleanly before the thaw (A7.3).

Non-LVM parts (the ESP, `/boot`) use Track mode in the same run.

**Why mirror mode is synchronous only on a LAN:** synchronous write latency is about the round trip plus the flush. Over a WAN, the forecast offers Track mode instead (XvMotion's lesson: async mirroring needs in-order replay and a careful handshake, which is future work).

### A5.4 Hypervisor accelerator (later)

If the user controls a QEMU/KVM host, the receiver can also speak NBD. QEMU's `blockdev-mirror` (background, then `write-blocking`, as Proxmox does it) pushes into the receiver's NBD front door. The receiver's applier and index work unchanged. NBD frames carry no sender hash, so per-chunk end-to-end hashing is replaced by the post-go-live source verify (A10). This mode needs no agent in the guest at all.

---

## A6. Convergence controller

**Inputs per pass:**
- bytes sent;
- pass duration;
- dirty bytes drained at the end of the pass;
- link throughput;
- receiver apply rate;
- the user's **downtime budget** (default 5 s of data downtime).

**Model:**
- `churn = drained_dirty / pass_duration`
- `rate = min(link, read, apply)`
- Predicted final delta after the freeze ≈ `dirty since last drain`, and its send time ≈ `final_delta / rate + rtt + flush`.

**Decision after each pass:**

| Condition | Action |
|---|---|
| Predicted data downtime ≤ budget | **Ready.** Hold here, sending small passes, until the user or the schedule says cut over |
| Dirty set shrank ≥ 20% over the last two passes | Continue (two passes, so one write burst doesn't trigger a reaction) |
| Not shrinking and Mirror is available | Offer **Mirror** (A5.3) |
| Not shrinking, no Mirror | Offer **Throttle**: cgroup v2 `io.max` on the top writers, stepped down until churn < 0.5 × rate, never below the user's floor |
| Still not shrinking at the floor | **Can't converge.** Report the forecast ("downtime would be about 48 s at current churn") and suggest a low-churn window. The user may still choose to cut over with the longer downtime |

The forecast is shown live: "Ready: predicted data downtime 2.1 s, total about 35 s."

---

## A7. Cutover and rollback

### A7.1 States

Both sides journal their state durably before acting on it. The **epoch** is a random 64-bit value chosen at pairing, written in both journals and in the export names, and part of every frame.

```
 Source (sender journal)                      Target (receiver journal)
 Syncing ─► Quiescing ─► FinalPass ─►        Receiving ─► Final ─► Committed ─► Finalizing ─► Booted ─► Verified
            │              │                                │
            ▼              ▼                                ▼
          Thawed ◄──── RollingBack ◄──────────────────── RolledBack
                         (only after target recorded RolledBack)
 FinalPass ─► Committed (source stays frozen, then powers off)
```

### A7.2 The fast cutover, step by step

1. **Gate:** the convergence controller says Ready, and the user (or schedule) confirms. The receiver confirms it is caught up (`acked_seq` = last sent).
2. **Quiesce (source):**
   - Run the app pre-freeze hooks (for example, `CHECKPOINT` and pause the pooler).
   - `swapoff -a`.
   - `mlockall`.
   - `FIFREEZE` every filesystem, deepest mount first.

   The watchdog thread thaws everything if the step from Quiescing to Committed exceeds the hard limit (default 60 s).
3. **Drain and check (source):** wait for in-flight I/O to finish: the frozen filesystems have drained, so there are no pending writes. Flip the generation twice; the second drain must be empty except for the agent's own sentinel. A non-empty second drain means a writer is still active (raw device, passthrough, unfrozen filesystem), so abort, thaw and report it (M6).
4. **Final pass:** send the last drained ranges, wait for the acknowledgement, and compute the source Merkle root.
   - The sender holds fresh hashes for every chunk it read this pass.
   - For untouched chunks it uses the receiver's index entries, whose correctness was established by earlier acknowledged, read-back-verified passes plus continuous tracking. U1's background verify checks this assumption later.
5. **Receiver Final:** compare roots. On mismatch, go back to a compare pass while still frozen, or roll back.
6. **Commit:**
   1. The receiver writes the receipt, journals `Committed(epoch)` and flushes. It does **not** boot yet (A7.4).
   2. The source, still frozen, journals `Committed(epoch)` to its own small tmpfs state plus the receiver's acknowledgement, then **powers off or is fenced**, emitting `SourceSurrendered(epoch)` to the receiver as the last thing it does. Its disks are frozen throughout, which is why the receiver's record is authoritative.
   3. Only on receiving `SourceSurrendered` does the receiver move to Finalizing (A8). A committed target that never saw the token stays down for that epoch for ever.

   *Optional fast-rollback window:* the user may keep the source frozen-but-powered instead of off; then `SourceSurrendered` is withheld until the window closes and the target waits. The target still never boots without the token.
7. **Finalize and boot (target):** A8.
8. **Network identity:** move the service IP:
   - same L2: gratuitous ARP;
   - cloud: re-assign the elastic or floating IP;
   - otherwise: DNS with a low TTL set at rehearsal time.

**Data downtime** is steps 2–6 (target ≤ 5 s). **Total downtime** adds steps 7–8.

**Verified-first variant** (MD4): before step 6, run a full compare pass of every used chunk while frozen. The receipt is then fully proven at commit, at the cost of minutes of downtime.

### A7.3 Rollback

Rollback is allowed until the target journals `Booted`, and afterwards only with an explicit "discard target writes."
1. The target journals `RolledBack(epoch)` and **refuses to boot** for that epoch ever after.
2. Only then does the source thaw: in mirror mode it detaches the remote leg first with `lvconvert -m0` on the source's own PV.
3. The source keeps running exactly as before. It was never written (M1).

### A7.4 Why there can't be two live copies (fail-closed fence)

The fence is **cooperative software, not fencing hardware or a quorum**, so it is designed to fail *closed on the target* and *alive on the source*. The target staying down is always safe (the source is intact and unmodified); two live copies never is. A network partition plus a human must not be able to produce split brain — the earlier "target boots on `Committed`, source thaws on `Rollback`" wording allowed exactly that if the final-pass link dropped while the watchdog fired.

- **The target boots only with *two* durable facts: its own `Committed` *and* a positive source-down token** — the source's `SourceSurrendered(epoch)` record, emitted only after the source has frozen, and powered off or been fenced. A `Committed` alone is **not** enough. Without the token the target refuses to boot for that epoch, for ever.
- **The source never thaws itself into a live state during cutover.** Its watchdog (I5.4) thaws only to **keep the source serving** when the cutover is abandoned, and only *before* it has emitted `SourceSurrendered`. Before surrender the source may always abort and keep running, because the target cannot have booted (no token). After surrender it powers off and does not return for that epoch.
- **Link drop during the final pass** (the old hole): the source has not surrendered, so its watchdog thaws it and it keeps running; the target never receives the token, so it never boots. No split brain and no coordination across the dead link.
- **Rollback** (A7.3) is the explicit operator path and still records the target's `RolledBack` before the source resumes — but it is now a *convenience*, not a safety dependency, because the automatic path already fails closed.
- A crash of either side resumes into one of these states. The exhaustive test in `plan.md` 3.5 checks every crash × message-loss combination for two invariants: never two writable copies, and always a safe way forward or back.

---

## A8. Target finalize

All steps are journaled and idempotent, like BootShift's executor steps.

1. **Layout (before the first byte):**
   - The plan maps each source partition to a target extent.
   - The default keeps every partition at its source offset and lets the last partition, or a chosen one, grow into the new space.
   - Moving partitions is allowed because the target is empty: the map is a list of (source range → target range).
   - GPT: same disk GUID and PARTUUIDs, new sizes, and the backup header at the new end of disk (written backup first, flushed, then primary, as in BootShift).
2. **Grow** with the BootShift `fs` adapters:
   - ext4: `resize2fs` offline in the receiver (grow is safe);
   - XFS: `xfs_growfs`, mounted in the receiver;
   - Btrfs: `btrfs filesystem resize max`;
   - LVM: `pvresize` and `lvextend -r`.

   Shrink is refused (non-goal).
3. **Guest changes for the new hardware.** These run through `bootshift-disk::guard`, so every byte changed is in the undo log:
   - initramfs: add the storage and network drivers of the target (virtio, nvme, ena, hv_*), using the guest's own `update-initramfs` or `dracut` in a chroot;
   - fstab and GRUB: rewrite device names to UUIDs where the old names won't exist;
   - network config: drop MAC-bound rules, keep the IP plan the user chose (keep, or switch to DHCP);
   - serial console on.

   The diff is shown and stored with the receipt.
4. **Boot:** `kexec_file_load` the replica's own signed kernel and initramfs with its command line. If kexec is refused, set the firmware boot entry (BootShift `efi`), reboot, and run a boot health check: SSH or agent heartbeat plus user-defined service checks. If the health check fails, the undo log can revert the guest changes for a retry. The source is still intact.

---

## A9. Rehearsal

1. At a pass boundary the applier pauses briefly.
2. The receiver creates a **writable dm-snapshot** of the data disk, with its COW store on the scratch disk or a separate disk, and exports it over NVMe/TCP or iSCSI.
3. The applier resumes writing to the origin through `snapshot-origin`, so the snapshot stays frozen at the rehearsal point.

A throwaway VM then boots the snapshot with the same guest-change steps, under a different hostname and an isolated network. The user tests apps, or tries `do-release-upgrade`, and reads the rehearsal report:
- boot time;
- health checks;
- the guest-change diff;
- measured timings for each cutover step.

Deleting the snapshot leaves the replica untouched; Phase 0.7 checks this through the hash index. If the COW store fills, the snapshot is dropped and the rehearsal stops. Replication never stops.

---

## A10. Proof after go-live

After `Committed`, the source is frozen or powered off and unchanged. The background verify:
1. reads every chunk of the source:
   - frozen: read in place;
   - powered off: boot it into the BootShift recovery image read-only, or attach its volume to a helper;
2. hashes it;
3. compares it with the **receipt's index snapshot**, not the live target, which is already taking new writes.

All equal: the receipt is marked `Verified` and the user can retire the source. A mismatch names the chunks, keeps the source, and offers rollback with an exact list of what differs. This gives U1's full proof without adding downtime.

---

## A11. Failure matrix

| Failure | When | What happens |
|---|---|---|
| Link drops | Any pass | Sender marks unacknowledged ranges dirty again and reconnects; receiver resumes at `acked_seq`; no data before it is resent |
| Receiver VM crashes / power cut | Any pass | Journal slot + intent log on resume; suspect ranges re-hashed after resend |
| Source agent crashes | Any pass | Tracker lost: next pass is a compare pass (sends only differences) |
| Source reboots | Any pass | Same as above; the agent restarts by hand or as a unit, and preflight runs again |
| Tracker loses an event / detaches / passthrough write | Any pass | Tracking broke: whole disk dirty, compare pass (M4) |
| Writer not stopped by freeze | Quiescing | Second drain not empty: abort, thaw, report the writer (M6) |
| Freeze hangs | Quiescing | Watchdog thaws; cutover aborted; source keeps running |
| Link drops | Final pass | Source has not surrendered: the watchdog thaws it and it keeps serving; the target never receives `SourceSurrendered`, so it never boots (A7.4). Retry later. No cross-partition coordination is needed |
| Receiver crashes | After `Committed` | Resumes at Finalizing (idempotent steps); source stays frozen/off |
| Target fails to boot | Booted health check | Undo the guest changes and retry; or rollback (target never served, so no data to lose) |
| Mirror leg drops | Mirror mode, before cutover | Source continues on its own leg; region resync on reconnect |
| Mirror leg drops | Mirror mode, during freeze | Not in sync: abort cutover, thaw, retry later |
| Background verify mismatch | After go-live | Named chunks, source kept, rollback offered |

---

## A12. Security

- **Pairing:** the receiver shows a 6-word code. The sender uses it as a PAKE secret (SPAKE2) to authenticate both sides and derive TLS 1.3 keys. There is no PKI to set up and nothing listens before the code is shown.
- **Encryption:** all data is encrypted in transit. NVMe/TCP and iSCSI exports in mirror mode stay on the paired link: bound to the agreed interface and protected by a host NQN or IQN derived from the pairing.
- **Receipt:** signed with the receiver's ed25519 key generated at pairing. The public key is printed on both sides.
- **Privileges:** the sender needs root for block reads, eBPF and freeze. It drops all capabilities it doesn't use after setup. The eBPF program is read-only on kernel state (no `bpf_probe_write_user`).
- **No data leaves the paired link:** no cloud service and no telemetry.

---

## A13. How it fits BootShift

| Need | Reused from BootShift | New |
|---|---|---|
| Durable, aligned raw I/O | `bootshift-disk::RealDevice`, `BlockDevice` | — |
| Crash testing | `MemDisk`, `CrashDisk`, `FaultyDisk` | `LiveDisk`, `FaultyLink` |
| Journal slots, CRC32C, versioning | `bootshift-core::journal` codec style | Receiver journal format (own magic and version) |
| Used ranges | `bootshift-fs` adapters, `CopyScope` | — |
| Layout and GPT | `bootshift-core` planner, `bootshift-disk::gpt` | Range map for the target |
| Grow | `bootshift-fs` | — |
| Guest changes with undo | `bootshift-disk::guard`, `undo_log` | Adaptation steps |
| Boot entries, recovery image | `bootshift-boot` | Receiver image variant, kexec |
| GUI / D-Bus | `bootshiftd`, `bootshift-gui` | Migrate screens and methods |

**Dependency direction:** `bootshift-core` ← `migrate-core` ← `migrate-track`, `migrate-net` ← `migrate-send`, `migrate-receive` ← `migrate-cli`. `migrate-receive` also uses `bootshift-disk`, `bootshift-fs` and `bootshift-boot`. Nothing in BootShift depends on `migrate-*`.
