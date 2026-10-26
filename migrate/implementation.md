# BootShift Migrate — Implementation

Crates, types, algorithms, formats and tests. This document refers to `architecture.md` sections as A*n* and to `plan.md` tasks by number. All repository rules in `CLAUDE.md` apply:
- typed errors in libraries;
- no `unwrap`/`expect`/`panic!` outside tests;
- units as newtypes;
- every tool run through `bootshift_fs::run`;
- tests before features.

---

## I1. Crates

```
migrate/
  plan.md  architecture.md  implementation.md
  crates/
    core/      bootshift-migrate-core     Pure logic: messages + codec, receipt, dirty-range set,
                                          convergence model, cutover state machines, layout map.
                                          No I/O, no unsafe.
    track/     bootshift-migrate-track    Tracker trait + backends: ebpf (aya loader), compare,
                                          lvm-mirror control. Talks to the kernel; no unsafe in Rust.
    track-bpf/ bootshift-migrate-track-bpf  The eBPF program (aya-ebpf, no_std, target bpfel).
                                          Built by `cargo xtask build-tracker`, embedded as bytes.
                                          Needs `unsafe` (decision MD2).
    net/       bootshift-migrate-net      Framing over a byte stream, pairing (SPAKE2), TLS (rustls),
                                          reconnect, back-pressure. FaultyLink lives here for tests.
    send/      bootshift-migrate-send     Source probe + preflight, reader, pass loop, quiesce,
                                          sender state file.
    receive/   bootshift-migrate-receive  Scratch store (journal, intent, index, receipt), applier,
                                          finalize (layout, grow, adapt, boot), rehearsal, exports.
    cli/       bootshift-migrate-cli      The `bootshift-migrate` binary: send, receive, status,
                                          rehearse, cutover, rollback, verify, finalize.
```

**Workspace:** add `"migrate/crates/*"` to the root `members`, except `track-bpf`, which is excluded and built for the BPF target by xtask (MD1, MD2).

**Dependency direction:**
- `bootshift-core` ← `migrate-core` ← `migrate-track`, `migrate-net` ← `migrate-send`, `migrate-receive` ← `migrate-cli`.
- `migrate-receive` also depends on `bootshift-disk`, `bootshift-fs`, `bootshift-boot`.
- `migrate-send` depends on `bootshift-disk` (`RealDevice` read-only) and `bootshift-fs` (used ranges, probe).

**New workspace dependencies:** all permissive licences, so all compatible with GPL-3.0-or-later.

| Need | Crate | Licence |
|---|---|---|
| eBPF loader | `aya` | MIT/Apache-2.0 |
| eBPF program | `aya-ebpf` (in `track-bpf` only) | MIT/Apache-2.0 |
| TLS | `rustls` (ring or aws-lc-rs provider) | MIT/Apache-2.0/ISC |
| Pairing | `spake2` | MIT/Apache-2.0 |
| Signing | `ed25519-dalek` | BSD-3 |
| Compression | `zstd` (level 1) or `lz4_flex` | MIT/BSD |
| Hashing | `blake3` (already present) | CC0/Apache-2.0 |
| Random | `rand` / `getrandom` | MIT/Apache-2.0 |

No async runtime. The sender and receiver are a few plain threads joined by bounded `std::sync::mpsc` channels. That keeps the code obvious and gives back-pressure for free.

---

## I2. Core types (`migrate-core`)

```rust
/// Bytes on a disk, always a multiple of the 4 KiB tracking block.
pub struct TrackBlock(u64);          // index of a 4 KiB block
pub struct DiskRange { start: u64, end: u64 }   // byte offsets, 4 KiB aligned

/// A random value fixed at pairing; part of every frame, journal record and export name.
pub struct Epoch(u64);

/// Frames are numbered from 1; the receiver acknowledges the highest durable one.
pub struct FrameSeq(u64);

pub struct DirtySet { /* sorted, merged DiskRanges */ }
impl DirtySet {
    pub fn mark(&mut self, range: DiskRange);
    pub fn mark_all(&mut self, disk_len: u64);
    pub fn take_extents(&mut self, max_extent: u64) -> Vec<DiskRange>; // ≤ 4 MiB pieces
}

pub enum PassKind { Bulk, Delta, Compare, Final }

pub struct PassStats {
    pub kind: PassKind,
    pub sent_bytes: u64,
    pub duration: Duration,
    pub dirty_at_end: u64,
    pub link_rate: u64,     // bytes/s measured over the pass
}

pub enum Advice {
    Continue,
    OfferMirror,
    OfferThrottle { target_churn: u64 },
    Ready { predicted_data_downtime: Duration },
    CannotConverge { predicted_data_downtime: Duration },
}

pub struct ConvergenceModel { budget: Duration, history: Vec<PassStats> }
impl ConvergenceModel {
    pub fn advise(&self) -> Advice;   // A6 table, pure
}
```

Cutover state machines are plain enums with a pure transition function. Every transition that has a side effect returns the action for the caller to do, and the caller journals the new state before doing it.

```rust
pub enum ReceiverState { Receiving, Final, Committed, AwaitingSurrender, Finalizing(FinalizeStep), Booted, Verified, RolledBack }
pub enum SenderState   { Syncing, Quiescing, FinalPass, Committed, Surrendered, RollingBack, Thawed }

pub fn receiver_next(state: &ReceiverState, event: ReceiverEvent) -> Result<(ReceiverState, Action), Refuse>;
```

The fence fails closed (A7.4): the receiver may leave `AwaitingSurrender` for `Finalizing` **only** on a durable `SourceSurrendered(epoch)` token from the sender; a `Committed` receiver that never received the token stays down for that epoch for ever. The sender reaches `Surrendered` only after it has frozen and powered off / been fenced, and before that its watchdog may always thaw it back to serving (never into a second live copy, because the target cannot have booted without the token). `plan.md` 3.5 tests this exhaustively.

The layout map is a list of `(source: DiskRange, target: DiskRange)` pairs, built with the `bootshift-core` planner. It is checked to be one-to-one and to keep every identity field (M9).

---

## I3. Wire protocol (`migrate-core::proto`, carried by `migrate-net`)

**Framing.** Each frame is `[len: u32 LE][kind: u8][epoch: u64][seq: u64][payload][crc32c: u32]`, all inside TLS. The CRC catches our own bugs; TLS already catches tampering. The maximum frame is 4 MiB + header.

| Kind | Direction | Payload |
|---|---|---|
| `Hello` | both | protocol version, role, software version, disk geometry (len, logical block, used ranges digest) |
| `Plan` | R→S | layout map, chunk size, budget, chosen mode |
| `Chunk` | S→R | `pass: u32, offset: u64, len: u32, hash: [u8;32], codec: u8, data` (hash over uncompressed bytes as read) |
| `Ack` | R→S | `acked_seq` |
| `IndexRequest` / `IndexPage` | S→R / R→S | range of 1 MiB chunks / their hashes |
| `PassEnd` | S→R | pass number, kind, stats |
| `Freeze` / `FinalRoot` | S→R | source Merkle root at freeze |
| `Commit` / `Committed` | S→R / R→S | epoch, roots, receipt digest |
| `Surrendered` | S→R | epoch — sent by the source as its last act after freezing and powering off/fencing; the only token that lets a `Committed` target boot (A7.4) |
| `Rollback` / `RolledBack` | either / R→S | epoch |
| `Export` | R→S | mirror mode: transport, address, NQN/IQN |
| `Error` | both | typed refusal: reason + fix |

**Versioning.** `Hello` carries `PROTOCOL_VERSION`. Any incompatible change bumps it, and the decoder keeps a test for each older version it still accepts (repository journal rule, applied here).

**Pairing.** The receiver prints a 6-word code from a 2,048-word list, about 66 bits.
1. Over a plain TCP connection, both sides run SPAKE2 with the code and derive `k`.
2. Each side generates a self-signed certificate for this run and sends its fingerprint, MACed with `k`.
3. TLS 1.3 then runs with each side pinning the other's fingerprint.

A wrong code fails the MAC and the connection closes. There are 3 attempts, then the receiver makes a new code.

**Back-pressure.** The sender keeps at most `W` unacknowledged bytes (default 256 MiB). The receiver acknowledges each batch, so a slow target slows the reader, not RAM.

---

## I4. Receiver on-disk formats (`migrate-receive::store`)

Everything is on the scratch device and written raw (A2, A4.1). Magics use the prefix `BSMG`. `FORMAT_VERSION = 1`.

| Region | Offset | Size | Record |
|---|---|---|---|
| Slot A / B | 0, 1 MiB | 4 KiB each | `magic "BSMGSLOT", version, seq, run_id, epoch, state, finalize_step, acked_seq, index_root[32], crc32c` |
| Plan | 2 MiB | ≤ 1 MiB | `magic "BSMGPLAN", version, run_id, len, payload (layout map, geometry), crc32c` |
| Intent ring | 3 MiB | 13 MiB, 4 KiB records | `magic "BSMGINTN", version, run_id, batch_seq, n, ranges[n], crc32c` |
| Hash index | 16 MiB | 32 B × chunks | raw blake3 per 1 MiB target chunk; valid only up to the root recorded in the newest slot |
| Receipt | end − 1 MiB | ≤ 1 MiB | signed canonical JSON (I4.1) |

**Slot rules:** two slots written alternately, each in its own 4 KiB block ≥ 1 MiB apart. The newest valid slot wins and a torn slot is ignored. This is the same scheme as `bootshift-core::journal`, so the codec style and tests are copied, not shared. The formats evolve separately.

**Index validity:**
- The index pages written before a slot commit are flushed before that commit.
- On resume, entries covered by intent records newer than the slot's `acked_seq` are marked suspect.
- A suspect entry is recomputed after the resent data is applied.

### I4.1 Receipt

```json
{
  "format": "bootshift-migrate-receipt/1",
  "run_id": "…", "epoch": "…",
  "source": { "host": "…", "disks": [{ "serial": "…", "len": 0, "used_digest": "…" }] },
  "target": { "disks": [{ "len": 0 }], "layout": [{ "source": [0, 0], "target": [0, 0] }] },
  "chunk_size": 1048576,
  "source_root": "blake3:…", "target_root": "blake3:…",
  "frozen_at": "2026-…Z", "committed_at": "…",
  "data_downtime_ms": 0,
  "guest_changes": [{ "file": "/etc/fstab", "diff": "…" }],
  "verified": null | { "at": "…", "result": "match" | { "mismatch": [ … ] } },
  "receiver_key": "ed25519:…", "signature": "…"
}
```

The JSON is canonical: keys sorted, no whitespace. That way anyone can recompute the digest. A `bootshift-migrate verify-receipt` command checks the signature and, given both disks, the roots.

---

## I5. Algorithms

### I5.1 Sender pass loop (`migrate-send::passes`)

```rust
fn run_passes(link: &mut Link, tracker: &mut dyn Tracker, source: &SourceDisks,
              model: &mut ConvergenceModel) -> Result<Readiness, SendError> {
    tracker.confirm_live()?;                       // M5: tracking before the first read
    let mut dirty = DirtySet::from(source.used_ranges()); // bulk pass = used ranges
    loop {
        let generation = tracker.flip()?;          // new writes land in the next generation
        let stats = send_ranges(link, source, dirty.take_extents(MAX_EXTENT))?;
        match tracker.drain(generation)? {
            Drained::Ranges(changed) => dirty.extend(changed),
            Drained::Broken(reason) => {           // M4
                log::warn!("tracking broke: {reason}; next pass compares hashes");
                return compare_pass(link, source, model);
            }
        }
        dirty.extend(link.unacknowledged_ranges()); // resend anything not yet durable
        model.record(stats, dirty.total_bytes());
        match model.advise() {
            Advice::Ready { .. } => return Ok(Readiness::Ready),
            Advice::Continue => continue,
            other => return Ok(Readiness::NeedsDecision(other)),
        }
    }
}
```

`send_ranges` runs a reader thread (`O_DIRECT`, 4 MiB extents, 4 in flight). It hashes each extent with blake3 over the bytes exactly as read, compresses it, and frames it. The sender never re-reads a chunk to send it. It sends what it hashed.

### I5.2 Receiver applier (`migrate-receive::apply`)

```rust
fn apply_batch(store: &mut Store, target: &mut dyn BlockDevice, batch: &[ChunkFrame])
    -> Result<FrameSeq, ApplyError> {
    store.write_intent(batch.iter().map(|c| c.target_range()))?;   // flushed inside
    for chunk in batch { target.write_at(chunk.target_offset(), &chunk.data)?; }
    target.flush()?;                                               // M3: flush ...
    for chunk in batch { verify_read_back(target, chunk)?; }       // ... verify ...
    let touched = index_chunks_touched(batch);
    store.update_index(target, &touched)?;                         // re-hash 1 MiB chunks, flush
    let last = batch.last().map(|c| c.seq).ok_or(ApplyError::EmptyBatch)?;
    store.commit(SlotUpdate::acked(last, store.index_root()))?;    // ... then record
    Ok(last)                                                       // caller sends Ack
}
```

A batch is ≤ 64 MiB or ≤ 50 ms of frames, whichever comes first. The CrashDisk test (4.2) crashes at every write and flush point inside `apply_batch` and checks the following, after the sender resends from the recorded `acked_seq`:
- the target equals the simulated source;
- the index equals the hashes of the target.

### I5.3 Drain protocol for the eBPF tracker (`migrate-track::ebpf`)

```rust
pub fn flip(&mut self) -> Result<Generation, TrackError> {
    let old = self.generation;
    self.control.set(GENERATION_KEY, old.next())?;   // programs now write buf[new & 1]
    self.generation = old.next();
    Ok(old)
}

pub fn drain(&mut self, generation: Generation) -> Result<Drained, TrackError> {
    if let Some(reason) = self.health.check()? { return Ok(Drained::Broken(reason)); }
    let buffer = &mut self.buffers[generation.slot()];
    let mut changed = Vec::new();
    for word_index in 0..buffer.len() {
        let word = buffer.get(word_index)?;
        if word != 0 {
            buffer.set(word_index, 0)?;    // a racing writer also set the bit in the new buffer
            changed.extend(ranges_of(word_index, word));
        }
    }
    Ok(Drained::Ranges(changed))
}
```

Reading the whole buffer is about 32 MiB per TiB. Batch lookups (`BPF_MAP_LOOKUP_BATCH`) keep this to milliseconds. A summary map holds 1 bit per 64 words and lets the drain skip clean areas.

**The BPF program** (`track-bpf`), in outline:

```rust
#[tracepoint]
pub fn on_bio_queue(ctx: TracePointContext) -> u32 { mark(&ctx, Event::Submit) }

fn mark(ctx: &TracePointContext, event: Event) -> u32 {
    let Some(disk) = tracked_disk(ctx.dev()) else { return 0 };
    if !changes_data(ctx.rwbs()) { return 0 }            // W, D (discard), Z (zeroes), E (erase)
    let generation = CONTROL.get(GENERATION_KEY);
    set_bits(disk, generation, ctx.sector(), ctx.nr_sector());
    let again = CONTROL.get(GENERATION_KEY);
    if again != generation { set_bits(disk, again, ctx.sector(), ctx.nr_sector()); }
    COUNTERS.increment(event);
    0
}
```

`set_bits` uses an atomic OR on the word (kernel ≥ 5.12; per-CPU maps merged in userspace below that). Ranges longer than the verifier's loop bound are split. If a range is too large to mark, the program sets the `overflow` counter, and the userspace side treats an overflow as "tracking broke".

**Passthrough writes.** A second program on `nvme:nvme_setup_cmd` and `scsi:scsi_dispatch_cmd_start` counts write opcodes to tracked disks that did not come from the block layer. Any count above zero means "broke".

### I5.4 Quiesce (`migrate-send::quiesce`)

1. Run the user hooks from `/etc/bootshift-migrate/hooks/pre-freeze.d/`, with a timeout each.
2. `swapoff -a`. If memory is short, refuse instead of thrashing (preflight already warned).
3. `mlockall(MCL_CURRENT | MCL_FUTURE)`. Pre-open every file and device the remaining steps need; after this point the sender does no filesystem I/O.
4. Start the watchdog thread with a monotonic deadline. On expiry it runs `FITHAW` on every frozen filesystem, in reverse order, and exits non-zero.
5. Run `FIFREEZE` on each mount point, deepest first, from `/proc/self/mountinfo`, skipping virtual filesystems. `EBUSY` on an already frozen filesystem is a refusal: someone else froze it.
6. Run drain, drain again, and check that the second drain holds only the sentinel. Anything else aborts (A7.2 step 3).

Thaw is the same list in reverse. Both directions are idempotent, so the watchdog and the main thread can't double-thaw into an error.

### I5.5 Mirror mode commands (`migrate-track::lvm_mirror`)

Every tool runs through `bootshift_fs::run`, with JSON output where it exists (`lvs --reportformat json`).

```
receiver: nvmet configfs: subsystem nqn.2026-10.org.bootshift:<epoch>, namespace = PV partition,
          port tcp <addr>:4420, allowed host = sender's host NQN
sender:   nvme connect -t tcp -a <addr> -n <nqn> --hostnqn <derived>   (or iscsiadm for LIO)
          pvcreate <dev> ; vgextend <vg> <dev>
          lvconvert -y --type raid1 -m 1 --regionsize 4M <vg>/<lv> <dev>     (for each LV)
          watch: lvs -o sync_percent,raid_sync_action,lv_health_status
cutover:  freeze → all LVs sync_percent 100 and health "" → Commit
rollback: lvconvert -y -m 0 <vg>/<lv> <dev> ; vgreduce <vg> <dev> ; nvme disconnect
target:   vgreduce --removemissing --force <vg>  ← one journaled step; then lvconvert -m 0
```

Each LV's raid1 conversion and each target-side cleanup is a separate journal step, so a crash in the middle resumes at the right one. Phase 0.6 must confirm the exact cleanup commands are idempotent on the LVM versions in Ubuntu 22.04 and 24.04.

### I5.6 Convergence model

Pure, so it is easy to property-test.

```rust
pub fn advise(&self) -> Advice {
    let Some(last) = self.history.last() else { return Advice::Continue };
    let rate = last.link_rate.max(1);
    let predicted = Duration::from_secs_f64(last.dirty_at_end as f64 / rate as f64) + self.fixed_cost;
    if predicted <= self.budget { return Advice::Ready { predicted_data_downtime: predicted } }
    if self.shrinking_over_last_two() { return Advice::Continue }
    if self.mirror_available { return Advice::OfferMirror }
    if !self.throttled_to_floor { return Advice::OfferThrottle { target_churn: rate / 2 } }
    Advice::CannotConverge { predicted_data_downtime: predicted }
}
```

`fixed_cost` is the measured round trip plus the receiver's flush time plus the freeze time from the last rehearsal, or a default before any rehearsal.

---

## I6. Target finalize (`migrate-receive::finalize`)

Steps are an enum journaled in the slot (`finalize_step`). Each one is idempotent.

| Step | What | Reused |
|---|---|---|
| `WriteTable` | GPT for the target layout: backup header first, flush, then primary, flush | `bootshift-disk::gpt` |
| `Grow(part)` | Grow each partition's filesystem or PV into its new size | `bootshift-fs` ext4/xfs/btrfs, `run` for LVM |
| `CleanupMirror` | Mirror mode only (I5.5) | — |
| `Adapt` | Guest changes through `guard` (undo log); the diff is stored in the receipt | `bootshift-disk::guard`, `undo_log` |
| `WriteReceipt` | Final signed receipt | — |
| `Boot` | `kexec_file_load`, or set BootNext + BootOrder and reboot | `bootshift-boot::efi` |

**Adapt** mounts the replica's root read-write in the receiver, bind-mounts `/dev`, `/proc` and `/sys`, and runs the guest's own tools in a chroot:
- `update-initramfs -u -k all` (Debian/Ubuntu) or `dracut -f --regenerate-all` (Fedora/RHEL), with `MODULES=` additions for the target's storage and network drivers;
- fstab and GRUB device-name rewrites, done by our code on parsed files, not with sed;
- network config: netplan or NetworkManager key-file edits for the IP plan the user chose.

Every file write goes through the guarded device, so `bootshift-migrate finalize --undo-adapt` restores the exact bytes.

---

## I7. Rehearsal (`migrate-receive::rehearse`)

1. Pause the applier after the current batch commits.
2. Reload the data disk's dm table as `snapshot-origin`, and create `snapshot <origin> <cow> P 8` on the COW device. The receiver owns the data disk, so it can stack dm freely.
3. Resume the applier, which now writes through the origin.
4. Export the snapshot device with nvmet under a different NQN.
5. On `rehearse --end`: remove the export and the snapshot, then reload the origin as linear. Check the hash index root is unchanged; a mismatch is a bug and stops the run.

The COW fill level is watched. At 90% the snapshot is removed and the rehearsal ends with a clear message. Replication never pauses for longer than one batch.

---

## I8. CLI (`bootshift-migrate`)

```
bootshift-migrate receive [--data /dev/vdb] [--scratch /dev/vdc | --scratch-end 2G]
    → prints pairing code, waits
bootshift-migrate send <code> --to <addr> [--budget 5s] [--mode auto|track|compare|mirror]
    → preflight, plan summary (mode, layout, forecast, what will be changed), confirm, run
bootshift-migrate status            → passes, churn, forecast, state on both sides
bootshift-migrate rehearse [--end]  → A9
bootshift-migrate cutover [--verified-first] [--at 02:00]
bootshift-migrate rollback          → A7.3
bootshift-migrate verify            → A10 background source verify; updates the receipt
bootshift-migrate verify-receipt <file> [--source DEV --target DEV]
```

Every refusal prints a reason and a fix, as in BootShift: `Refuse { reason: "Swap is in use and only 300 MiB of RAM is free", fix: "Stop memory-hungry services or add RAM before cutover" }`.

---

## I9. Tests

The repository rule applies: the test infrastructure is built before the feature it tests.

| Level | What | Where |
|---|---|---|
| Unit + proptest | Codec round-trip; dirty set; convergence model (never Ready above budget; terminates for any fixed churn); layout map one-to-one; receipt canonical form | `migrate/crates/core` |
| State machines | Exhaustive: every crash point on either side × every message loss → never two writable copies; always a way forward or back (plan 3.5) | `core/tests/cutover.rs` |
| Applier crash | `CrashDisk` at every write and flush point with dropped, reordered and torn pending writes; resume; target equals source and index equals target (plan 4.2) | `receive/tests/apply_crash.rs` |
| End-to-end sim | `LiveDisk` (seeded writer) + `SimTracker` + `FaultyLink` + receiver on `CrashDisk`: random write schedules, link cuts, tracker breaks. Final check: target equals the `LiveDisk` image at freeze. Seeds are printed and failures kept as regression cases | `receive/tests/end_to_end.rs` |
| Drain race | Every interleaving of mark (set, re-check, set) with flip and drain (loom-style) | `track/tests/drain_race.rs` |
| Fuzz | Frame decoder, slot decoder, intent decoder, receipt parser | `crates/*/fuzz/` |
| VM | Two QEMU guests (`cargo xtask migrate-vm-test`): fio at 4 churn levels, pgbench + `pg_amcheck` after cutover, kill either VM at random (1,000 runs), partition the network at every state, dm-dust on the source | `xtask` |
| Tracker truth | blktrace vs eBPF bitmap superset check under fio, LVM, dm-crypt, md and swap; passthrough detection (plan 0.1) | VM only, `#[ignore]` + `require_test_vm()` |

Privileged tests follow the existing guard: `#[ignore]`, `require_test_vm()`, loop or virtio devices only.

---

## I10. Build and toolchain

- The `track-bpf` crate builds with `cargo +nightly build -Z build-std=core --target bpfel-unknown-none` plus `bpf-linker`, wrapped as `cargo xtask build-tracker`. Its output (`tracker.bpf.o`) is embedded into `migrate-track` with `include_bytes!` from `xtask/out/`, so normal builds stay on the pinned stable toolchain.
- CO-RE: the program reads only tracepoint context fields, so it needs no kernel headers at runtime. BTF is checked at preflight.
- The release binary is static musl like `bootshift`: `cargo build -p bootshift-migrate-cli --release --target x86_64-unknown-linux-musl`.
- The receiver image is the BootShift recovery initramfs plus `nvmet`, `nvme_tcp`, `iscsi_target_mod`/`target_core_*`, `dm_snapshot`, `dm_raid` and the migrate binary. It is built by `bootshift-boot::initramfs` with an extra module list, and shipped as an ISO and a raw cloud image (`cargo xtask build-receiver-image`).

---

## I11. Logs and progress

- One log line per pass: kind, bytes sent, duration, dirty at end, link rate, advice.
- One log line per state transition on each side, including the epoch.
- Progress for the GUI goes through the same `ProgressSink` idea as BootShift's copier, extended with `pass(stats)` and `forecast(advice)`.
- The receipt is the permanent record. Logs are for debugging only.

---

## I12. Order of building

Follow `plan.md` §5. The first useful milestone is Phases 0–5 plus Compare mode (6.2) and a manual cutover (7.1–7.2). That gives a correct, verifiable migration with minutes of downtime and no tracker. Track mode (6.1) then cuts downtime to seconds, and mirror mode (Phase 9) handles high-churn LVM servers. Each mode is useful on its own, and each one later is only faster, never less safe.
