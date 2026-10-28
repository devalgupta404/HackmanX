# Contributing to BootShift

Thanks for your interest in BootShift. It moves and resizes disk partitions, so the bar for changes is high: a bug here can destroy someone's data. Please read this before opening a pull request.

## The one rule

**Never lose or corrupt user data.** If a change trades safety for speed, simplicity, or a nicer demo, choose safety. If you are unsure whether something is safe, open an issue and ask before writing code.

Every change must keep all of the [safety invariants](#safety-invariants) true, even if the tests pass.

## Ground rules for working on this repo

- **Never run anything that writes to a real disk on your machine.** No `dd`, `sfdisk`, `mkfs`, `resize2fs`, `ntfsresize`, `efibootmgr`, `bootshift apply`, or `bootshift recover` against `/dev/sd*`, `/dev/nvme*`, or `/dev/mmcblk*`. Destructive work happens only in `MemDisk` / `CrashDisk` or on loop devices inside the test VM (`cargo xtask vm-test`).
- **Never weaken, skip, or `#[ignore]` a safety check or crash test to make something pass.** Fix the cause, or report it.
- **Never change the on-disk journal format without bumping its version,** and keep a decode test for the old version.

## Getting set up

You need a recent stable Rust toolchain (pinned in `rust-toolchain.toml`) and, for the musl release build, `musl-tools`.

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # unit, proptest, CrashDisk; fast
cargo deny check                  # licences and advisories
```

Privileged and end-to-end work runs in a QEMU guest, never on the host:

```bash
cargo xtask prepare-vm            # build the provisioned guest image (once)
cargo xtask vm-test               # privileged tests inside the guest
cargo xtask boot-proof            # apply, restart, power-cut mid-move, resume
```

Run `cargo fmt`, `cargo clippy`, and `cargo test --workspace` before you say a change is done. If your change touches `disk`, `engine`, `fs`, or `boot`, also run `cargo xtask boot-proof`.

## Testing

Tests are written **before** the feature they test. A change is not done without them.

- **Pure logic (`core`):** unit tests, plus `proptest` properties for anything with a large input space (the planner especially).
- **Anything that writes to a disk:** a `CrashDisk` test that crashes at **every** recorded write/flush point, resumes, and checks the final bytes. Dropped, reordered, and torn (512 B) pending writes must all be covered.
- **Filesystem adapters:** unit tests over captured tool output (fixtures in `crates/fs/tests/fixtures/`), plus a VM test on a real image.
- **Parsers of on-disk or external data** (journal, GPT, tool output): a `cargo fuzz` target.
- **Bug fixes start with a failing test** that reproduces the bug.

Tests are deterministic. Random tests print their seed, and a failing seed is added as a regression case.

## Code style

The goal is code a new contributor understands on the first read.

- Clear names over comments; name things after the domain (`move_partition`, `next_chunk`, `hash_chain`).
- Small functions that do one thing. Straight-line code over clever code.
- Types encode units: sector counts and LBAs are newtypes (`Lba`, `Sectors`), never raw `u64` mixed with bytes.
- Comments explain only the non-obvious *why* (a hardware quirk, an ordering constraint). No comments that restate the code. Every `unsafe` block gets a one-line `// SAFETY:` comment.
- Libraries return typed errors with `thiserror`; binaries use `anyhow` with `.context()`.
- No `unwrap`, `expect`, or `panic!` in non-test code of `core`, `disk`, or `engine` (enforced by clippy).
- `rustfmt` defaults, 100 columns. `clippy` clean with `-D warnings`. Imports grouped std / external / crate.
- Prefer an existing, maintained crate or a standard system tool over writing your own. Filesystem work shells out through `fs::run` (never a raw shell), preferring JSON output.

## Commits and pull requests

- **Commits:** small and focused, in the imperative mood, prefixed with the crate name: `copier: commit journal only after read-back verify`.
- **Don't mix** refactors with behaviour changes in the same PR.
- **PR description:** what changed and why, which safety invariants it touches, and which tests prove it.
- **No attribution trailers.** Please don't add `Co-Authored-By:`, "Generated with ...", or any other tool attribution to commits or PRs.
- By contributing, you agree your contribution is licensed under the project's [GPL-3.0 license](LICENSE).

## Where things go

- Unit tests sit next to the code in a `#[cfg(test)] mod tests`.
- Integration tests go in each crate's `tests/` folder; cross-crate end-to-end tests live in `crates/engine/tests/`.
- Captured tool output used as test input lives in `crates/<crate>/tests/fixtures/`.
- Fuzz targets live in `crates/<crate>/fuzz/`.
- Generated disk images and VM files are never committed; `xtask` writes them to `xtask/out/` (gitignored).

The ordered plan of record is [`task.md`](task.md); known defects and gaps are in [`issues.md`](issues.md). A good first contribution is often an item from `issues.md` with a failing test attached.

## Safety invariants

Every change must keep all of these true:

1. **Refuse before writing.** All preflight checks pass before the first byte on disk changes. A check that can't decide counts as a refusal.
2. **Data first, table last.** The partition table changes only after a partition's data is fully copied and verified in its new place.
3. **Flush, then record.** A chunk is marked done in the journal only after `fdatasync` returns on the device.
4. **One unrecorded write at a time.** No write for chunk *i+1* is issued until chunk *i*'s journal record is durable.
5. **Chunk ≤ distance.** Chunk size = `min(64 MiB, move distance)`, so a half-written chunk never overlaps its own source.
6. **Direction rule.** Moving left copies the first chunk first; moving right copies the last chunk first.
7. **Verify before commit.** Each chunk is read back with `O_DIRECT` and compared with the in-memory hash before it is journaled.
8. **The journal is the source of truth.** Every step is idempotent and resumes from the journal alone.

The full list, with the filesystem and boot details behind them, is in the architecture section of the project docs.
