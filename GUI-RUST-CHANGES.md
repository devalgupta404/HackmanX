# The Rust desktop app: what changed and why

This branch adds `bootshift-gui`, a native Rust desktop app built from `UI-HANDOFF.md`. It
replaces the need for a Tauri/webkit2gtk shell: the GUI is one Rust binary that talks only to
`bootshiftd`. The TypeScript frontend in `gui/` stays as the reference it was built from.

## Commits

1. `core: share plan wording and byte sizes between the CLI and the GUI`
2. `gui: add the native desktop app built from the UI handoff`
3. `docs: describe the desktop app and the helper's missing check rows`

## 1. Shared wording in `core`

The handoff requires the plan screen to say exactly what the command line says. Instead of
copying the CLI's sentences into the GUI, they moved into `core`:

- `Operation::describe(&Layout)` in `crates/core/src/plan.rs`: "Shrink partition 3 "Windows"
  to 116.5 GiB", "Move … (copies about …)", and so on.
- `units::format_bytes(u64)` in `crates/core/src/units.rs`: `476.9 GiB`.
- `crates/cli/src/commands.rs` now calls both. The CLI's output is unchanged; its size test
  moved to `core` with the function, and `describe` gained four tests.

This is a refactor with no behaviour change, kept in its own commit.

## 2. The desktop app, `crates/gui`

Package `bootshift-gui`, binary `bootshift-gui`, built with egui/eframe 0.37 (MIT/Apache).

### How it is put together

| File | What it does |
| --- | --- |
| `main.rs` | Picks the backend (helper, or demo with `--demo` or when the helper is absent) and opens the window. |
| `app.rs` | All state and the flow between the five screens. Screens return `Action`s that are applied after drawing. |
| `backend.rs` | The `Backend` trait, the D-Bus client for `org.bootshift.Helper1`, and a worker thread so the window never waits on the helper. |
| `protocol.rs` | The reply JSON. Layouts, targets, operations and refusals are `core`'s own serde types, so the contract can't drift. |
| `demo.rs` | The made-up 512 GB disk from handoff section 17, its plan and its twelve checks. |
| `editing.rs` | The snap-and-clamp rules for every drag and key press (handoff section 15). |
| `disk_bar.rs` | The disk bar: blocks, used fill, edge handles, grip, slivers, capsules and leader lines, the scale, dragging and arrow keys. |
| `gutter.rs` | The dimension lines between the two bars. |
| `table.rs`, `grid.rs` | The partition table, and a small CSS-grid column solver so header and rows line up. |
| `screens.rs`, `readout.rs`, `shell.rs` | The five screens, the right-hand readout, and the rail, action bar and responsive layout. |
| `console.rs`, `recovery_run.rs` | The recovery console and the timed demo run with its power cut. |
| `save.rs` | Saving a readable copy of the partition table to a file the user names. |
| `theme.rs`, `widgets.rs`, `motion.rs` | Colour and type tokens, the painted buttons, panels and refusals, and reduced-motion detection. |

### Safety properties

- **It never touches a disk.** It depends only on `bootshift-core` and asks the helper for
  everything. The helper re-plans and re-checks every request anyway.
- **Edits always stay valid.** The clamping mirrors `core::validate`, and a proptest drives
  random sequences of drags and checks that `validate` accepts every result. That test found
  a real bug, which is fixed here and saved as a regression case: moving a partition after
  resizing it could leave its end off a 1 MiB boundary. Such a move now does nothing, as the
  handoff requires ("a drag that cannot move must be a no-op, never a jump").
- **Apply is gated** on a non-empty plan with nothing unavailable, every check passing and
  the backup box ticked.
- **Demo mode is always labelled.** When the helper is missing the app says so on stderr and
  the rail shows "Demo disk. Nothing here touches hardware." on every screen.
- **Saving never overwrites.** The table backup refuses a file that already exists.

### Fidelity to the handoff

- Archivo, JetBrains Mono and Ubuntu Mono are bundled in `crates/gui/assets/fonts` with
  their licence texts. Archivo and JetBrains Mono are variable fonts, and each weight sets
  the `wght` axis. ✓ and ✕ are drawn as strokes, because none of the faces has a ✓.
- Colour rules hold: amber only for what is interactive or changed, green only for verified,
  red only for stop, and a disabled primary button is grey, not amber.
- Layout regions, sizes and copy follow the handoff. Where it differs from the TypeScript,
  the TypeScript wins, as the handoff says.
- Every control is keyboard-reachable with a visible amber focus ring, and every custom
  widget has an AccessKit name for screen readers.

### Bugs found while building it (all fixed)

- Drags lagged by egui's drag threshold, because the start was measured where the threshold
  was crossed instead of where the button went down.
- Shift+arrow moved a partition instead of resizing it, because a plain-arrow match also
  accepts Shift.
- The first arrow key after Tab moved focus away instead of the partition, because egui only
  takes a focus lock from a widget that was focused the frame before.
- Edge handles were extra Tab stops; they are pointer-only now.
- Actions showed one frame late until the next mouse move.

### Tests

64 tests in the crate, all passing, plus the rest of the workspace:

- Unit tests for the editing rules, plan reading, demo plan, formatting, grid, gutter labels,
  table change column, capsule placement, recovery-run timings, console lines and saving.
- The `editing` proptest against `core::validate` (also run at 20,000 cases).
- Nine end-to-end tests in `ui_tests.rs` with `egui_kittest`, sending real pointer and key
  events to the app:
  - the whole demo flow: drag, plan, checks, backup box, apply, preview, power cut, resume,
    done, and plan another;
  - arrow keys move and Shift+arrows resize by 1 GiB;
  - impossible drags and locked partitions change nothing;
  - a sliver is reached through its capsule;
  - table selection toggles;
  - steps ahead can't be skipped to;
  - tab order follows the disk;
  - nothing overflows at 1600, 1180, 1100 and 780px.

`BOOTSHIFT_SNAPSHOTS=xtask/out cargo test -p bootshift-gui ui_tests` also saves each screen as
a PNG (rendered headless with wgpu).

### Dependencies

- `eframe` 0.37 (glow, Wayland and X11, no system dev packages needed to build).
- `egui_kittest` 0.37, tests only.
- `zbus` and `serde_json` were already in the workspace.
- `deny.toml` gains one exception: `OFL-1.1` and `Ubuntu-font-1.0` for `epaint_default_fonts`
  only, the crate that ships egui's built-in fonts. `cargo deny check` passes.

## 3. Docs

- `CLAUDE.md`: `crates/gui` in the layout and dependency direction, and the commands to run
  and snapshot it.
- `README.md`: how to run the desktop app, and the layout table.
- `task.md`: new task 13.6 for the Rust app.
- `issues.md`: I-04 updated, and new I-17.

## Known gaps

- **Never run against a live `bootshiftd`.** The D-Bus client is written but has only met the
  demo backend; that needs the helper installed with root and polkit (I-04).
- **I-17: the helper's `Check` doesn't list its checks.** It returns `null` when everything
  passes, not one row per check as handoff section 16 says. The app then shows no rows and
  keeps apply locked: safe, but apply can't be reached against the real helper until `Check`
  returns the rows from `engine::preflight::evaluate`.
- **The table backup uses an in-app path box**, not the desktop's file picker, to avoid a GTK
  dependency.
- **No reagentc / chkdsk notes** on the done screen yet (task 13.4, I-14).

## Running it

```bash
cargo run -p bootshift-gui -- --demo     # the demo disk
cargo run -p bootshift-gui               # the real helper if bootshiftd is running, else the demo
cargo test -p bootshift-gui
```
