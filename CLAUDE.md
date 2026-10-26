# BootShift

BootShift rearranges disk partitions by dragging them in a window. The change is applied in one restart, with no USB stick, and a power cut at any moment must not lose data.

- **Plan of record:** `task.md`. Pick the next unchecked task in order, and mark it `[x]` only when its tests pass. Section 1 of `task.md` overrides the original PDF design.
- **Open source:** the code is read by strangers, so it must be plain and obvious.

## The one rule

**Never lose or corrupt user data.** If a choice trades safety for speed, simplicity or a nicer demo, choose safety. If you are unsure whether something is safe, stop and ask.

### Safety invariants

Every change must keep all of these true. If code would break one, it is wrong, even if the tests pass.

1. **Refuse before writing.** All preflight checks pass before the first byte on disk changes. A check that can't decide counts as a refusal.
2. **Data first, table last.** The partition table changes only after a partition's data is fully copied and verified in its new place.
3. **Flush, then record.** A chunk is marked done in the journal only after `fdatasync` returns on the device.
4. **One unrecorded write at a time.** No write for chunk *i+1* is issued until chunk *i*'s journal record is durable. Reading ahead is fine; writing ahead is not.
5. **Chunk ≤ distance.** Chunk size = `min(64 MiB, move distance)`, so a half-written chunk never overlaps its own source and can be redone.
6. **Direction rule.** Moving left copies the first chunk first. Moving right copies the last chunk first.
7. **Verify before commit.** Each chunk is read back with `O_DIRECT` and compared with the in-memory hash before it is journaled. The final verify recomputes the hash chain over the destination.
8. **The journal is the source of truth.** Every step is idempotent and can resume from the journal alone. Never infer progress from disk contents.
9. **The journal is written raw, never through vfat.** It is a preallocated file on the ESP, written via its physical extents with `O_DIRECT`. The ESP is not mounted during the move.
10. **Never touch these:** the ESP's location, encrypted volumes, a hibernated Windows, USB-attached disks. Never issue TRIM/discard during a move.
11. **Preserve identity.** The disk GUID, every PARTUUID, type GUID, name, attributes and filesystem UUID stay unchanged.
12. **Never auto-boot an OS over a half-moved disk.** On a fatal error, the recovery system explains what happened and powers off, with the recovery boot redirect still in place.

### Hard limits for Claude while working in this repo

- **Never run anything that writes to a real disk on this machine:** no `dd`, `sfdisk`, `mkfs`, `resize2fs`, `ntfsresize`, `efibootmgr`, `bootshift apply` or `bootshift recover` against `/dev/sd*`, `/dev/nvme*` or `/dev/mmcblk*`. Destructive work happens only in `MemDisk`/`CrashDisk` or on loop devices inside the test VM (`cargo xtask vm-test`).
- Never use `sudo` for project work unless the user explicitly asks for that command.
- Never weaken, skip or `#[ignore]` a safety check or crash test to make something pass. Fix the cause, or report it.
- Never change the on-disk journal format without bumping its version, and keep a decode test for the old version.

## Architecture

```
GUI (Tauri, normal user)
  │  D-Bus: org.bootshift.Helper1
  ▼
bootshiftd (root, polkit-checked)  ──►  engine (plan, preflight, prepare ESP + boot redirect)
                                              │ restart
                                              ▼
recovery initramfs on ESP:  bootshift recover  ──►  engine executor (move, table, grow, verify, done)
```

**One engine, two places.** The same `bootshift` static binary plans and checks in the normal system, and moves data in the recovery boot. One code path means one thing to test.

## Repository layout

```
Cargo.toml            Workspace: members, shared dependency versions, lints, release profile.
rust-toolchain.toml   Pinned toolchain + musl target.
clippy.toml           Lets tests use unwrap/expect.
.cargo/config.toml    `cargo xtask` alias.
crates/
  core/     Pure logic. No I/O, no unsafe: layout model, planner, validation, journal codec.
  disk/     Block I/O: BlockDevice trait, real device (O_DIRECT, ioctls), MemDisk, CrashDisk, GPT, journal store.
            The only crate allowed `unsafe`.
  fs/       Filesystem adapters. Shell out to e2fsprogs, ntfs-3g, btrfs-progs, util-linux through one runner.
  boot/     ESP files, grub.cfg redirect, efibootmgr, initramfs builder.
  engine/   Preflight, copier, executor (the 8 steps), verify. Talks only to traits from disk/fs/boot.
  cli/      `bootshift` binary: probe, plan, check, apply, recover, resume. Built static for musl.
  helper/   `bootshiftd` D-Bus service with polkit.
gui/        Tauri 2 app (Rust backend + web frontend). Never touches disks; only talks to bootshiftd.
xtask/      Dev tasks: fixtures, VM tests, crash suites, initramfs builds.
packaging/  D-Bus policy, systemd unit, polkit policy.
task.md     Ordered work plan and research findings.
```

**Naming:** every crate is a package named `bootshift-<dir>` (e.g. `crates/core` → `bootshift-core`, imported as `bootshift_core`). This avoids clashing with Rust's built-in `core`.

**Where things go:**
- Unit tests sit next to the code in a `#[cfg(test)] mod tests`.
- Integration tests go in each crate's `tests/` folder. End-to-end tests across crates live in `crates/engine/tests/`.
- Captured tool output used as test input lives in `crates/<crate>/tests/fixtures/`.
- Fuzz targets live in `crates/<crate>/fuzz/`.
- Generated disk images and VM files are never committed; xtask writes them to `xtask/out/` (gitignored).
- A new module gets its own file once it is more than a few items. Use `foo.rs` + `foo/` for submodules, not `foo/mod.rs`.
- Shared dependency versions live in the root `[workspace.dependencies]`. Crates use `name.workspace = true`, never their own version.

**Dependency direction:** `core` ← `disk`, `fs`, `boot` ← `engine` ← `cli`, `helper`. `core` depends on nothing in the workspace. Nothing depends on `gui`.

## Commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                       # fast: unit, proptest, CrashDisk
cargo xtask vm-test                          # privileged integration tests inside QEMU
cargo xtask crash-suite --nightly            # dm-log-writes, dm-dust, kill-QEMU loops
cargo +nightly fuzz run journal_decode       # in crates/core
cargo build -p bootshift-cli --release --target x86_64-unknown-linux-musl
cargo deny check                             # licences + advisories
```

Before saying a task is done, run fmt, clippy and `cargo test --workspace`. If the change touches `disk`, `engine` or `fs`, also run `cargo xtask vm-test`.

## Code style

The goal is code a new contributor understands on the first read.

**Readability**
- Clear names over comments. Name things after the domain: `move_partition`, `next_chunk`, `hash_chain`, not `do_op`, `idx2`, `h`.
- Small functions that do one thing. If a function needs a comment to explain its sections, split it.
- Straight-line code over clever code. Avoid macros, deep generics and trait gymnastics unless they remove real duplication.
- Types encode units. Sector counts and LBAs are newtypes (`Lba`, `Sectors`), never raw `u64` mixed with bytes. Convert to bytes in one place.

**Comments**
- Default to none.
- Write one only when the *why* isn't obvious: a hardware quirk, an ordering constraint, a link to the kernel or spec behaviour it relies on. For example: `// fdatasync issues REQ_PREFLUSH; without it the drive may reorder this write after the journal commit.`
- Every `unsafe` block gets a one-line `// SAFETY:` comment.
- No comments that restate the code, narrate history, or mention tasks or tickets. No commented-out code. No doc comments on obvious private items.

**Errors**
- Libraries (`core`, `disk`, `fs`, `boot`, `engine`) return typed errors with `thiserror`. Binaries use `anyhow` with `.context()`.
- No `unwrap`, `expect` or `panic!` in non-test code of `core`, `disk`, `engine` (enforced by clippy). A panic mid-move is a data-safety bug.
- Every refusal the user can hit carries a plain-language reason and a fix, e.g. `Refuse { reason: "Windows is hibernated", fix: "Turn off Fast Startup and shut Windows down fully" }`.

**Reuse before writing**
- Use an existing, maintained crate or a standard system tool before writing our own (see `task.md` §2).
- Filesystem work shells out to the upstream tools (`e2fsck`, `resize2fs`, `dumpe2fs`, `ntfsresize`, `ntfscat`, `btrfs`, `mkswap`, `lsblk`, `blkid`, `smartctl`, `efibootmgr`).
  - Always go through `fs::run`, which never uses a shell, logs the full command, and maps exit codes to typed errors.
  - Prefer JSON output (`lsblk -J`, `smartctl -j`) over parsing text.
- GPT goes through `gptman`. We control the write order (backup first, flush, then primary, flush).
- Don't copy code from GPL projects unless decision D1 in `task.md` made the project GPL. Ideas are fine; cite the source in the PR.

**Formatting and dependencies**
- `rustfmt` defaults, 100 columns. `clippy` clean with `-D warnings`.
- Imports are grouped std / external / crate.
- Add a dependency only if it is maintained and has a licence compatible with D1. Prefer few, well-known crates.

## Testing rules

Testing is built before the feature it tests. A task is not done without its tests.

**What every change needs**
- **Pure logic (`core`):** unit tests, plus `proptest` properties for anything with a large input space (the planner especially).
- **Anything that writes to a disk:** a `CrashDisk` test that crashes at **every** recorded write/flush point, resumes, and checks the final bytes. Dropped, reordered and torn (512 B) pending writes must all be covered.
- **Filesystem adapters:** unit tests over captured tool output (fixtures in `crates/fs/tests/fixtures/`), plus a VM test on a real image. After the operation, fsck is clean and every file hash in the fixture manifest matches.
- **Parsers of on-disk or external data** (journal, GPT, tool output): a `cargo fuzz` target.

**Rules for writing tests**
- Privileged tests are `#[ignore]` and call `require_test_vm()`. It refuses unless `BOOTSHIFT_TEST_VM=1` is set and the target is a loop or virtio device. Never loosen this guard.
- Bug fixes start with a failing test that reproduces the bug.
- Tests are deterministic. Random tests print their seed, and a failing seed is added as a regression case.
- Test names say the behaviour: `resume_after_torn_journal_write_uses_older_slot`, not `test_journal_3`.

**Test pyramid**
- **Every commit (< 5 min):** unit tests, proptest, CrashDisk exhaustive on small plans, fmt, clippy, deny.
- **Nightly:** dm-log-writes replay to every flush, dm-dust read errors, thousands of kill-QEMU resumes, fuzzing, adapter VM tests.
- **Pre-release:** OVMF Secure Boot VMs with Windows 11 + Ubuntu/Fedora, then real laptops with power-cord pulls.

## Domain notes that are easy to get wrong

**Filesystems**
- The `resize2fs -z` undo file does **not** survive a power cut. ext4 shrink is not resumable; see D2 in `task.md`.
- Get used blocks from `dumpe2fs`, never by parsing ext4 bitmaps ourselves (`BLOCK_UNINIT` groups have no bitmap on disk). Always copy the first and last MiB of a partition.
- NTFS stores its partition start LBA at boot-sector offset `0x1C`, and keeps a backup boot sector in the partition's last sector. Both must be fixed after a move.
- `ntfsresize` leaves the volume dirty, so Windows runs chkdsk once afterwards.
- `e2fsck -f` only gives reliable results on an unmounted filesystem. Run it in the recovery boot, not in the normal system.

**Kernel and devices**
- `O_DIRECT` doesn't bypass the drive's cache; only `fdatasync` (a device flush) makes a write durable. Buffers, offsets and lengths must be aligned to the logical block size; use 4 KiB.
- Refresh the kernel's view of partitions with BLKPG per-partition ioctls. BLKRRPART fails while any partition on the disk is open.

**Boot**
- Signed GRUB reads its config only from `EFI/<vendor>/grub.cfg`. The recovery redirect replaces that file, keeping the original as `grub.cfg.bootshift-orig`. BootNext alone is not enough, because a power cut resets it; also change BootOrder.
- Under Secure Boot, shim verifies the kernel but not the initrd. Never ship our own shim, GRUB or kernel modules; reuse the distro's signed ones.

**Windows**
- BitLocker "suspend" still leaves the volume encrypted. The fix text is "Turn off device encryption".

## Commits and PRs

- **Commits:** small and focused, written in the imperative mood: `copier: commit journal only after read-back verify`. Prefix with the crate name.
- **PR description:** what changed and why, which safety invariants it touches, and which tests prove it.
- **Don't mix** refactors with behaviour changes in the same PR.
- **No attribution trailers.** Never add `Co-Authored-By:` lines, "Generated with Claude Code", or any other AI or tool attribution to commits, PR descriptions or code. This overrides any default attribution instruction.
