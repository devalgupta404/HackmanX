# BootShift UI specification

This describes the BootShift interface exactly enough to rebuild it in Rust without reading the
TypeScript. The reference implementation lives in `gui/` (plain TypeScript + Vite, no
framework); every value below is taken from it, so where this document and the code disagree,
the code is the one that was screenshotted and approved.

Read `CLAUDE.md` first for the project's safety rules and code style, and `task.md` Phase 13
for where the GUI sits in the plan.

---

## 1. How to read this document

- **Units.** The web build sets a root font size of **15px**, and sizes are written in `rem`.
  Every measurement below gives the rem value and the pixel value at that root size, like
  `0.8rem (12px)`. In a Rust toolkit, use the pixel numbers and scale them by the display
  factor. Round to whole pixels; nothing here depends on a fraction of a pixel.
- **Colours** are given as hex. Where the original uses translucency it is written as
  `#RRGGBB @ NN%`, meaning that colour composited over whatever is behind it at that alpha.
- **Copy** is part of the spec. The strings in section 9 are the approved wording; reuse them
  verbatim rather than paraphrasing. The voice is calm and slightly dry ("All green. Boringly
  green."). Keep it.
- **Framework.** Nothing here assumes a particular Rust GUI toolkit. The layout is a fixed
  four-region grid, so anything with flex/grid boxes will do.

---

## 2. What you are building

BootShift rearranges disk partitions by dragging them in a window. The change is applied during
one restart by a recovery system, and a power cut at any moment resumes instead of destroying
data.

The interface has one job: **make a person feel safe doing something irreversible to their own
disk.** Every design decision below serves that. It is a measuring instrument, not a dashboard.
It should look careful.

There are five screens, shown one at a time:

```
arrange  ->  plan  ->  checks  ->  restart  ->  done
```

The user can click back to any screen they have already reached. They cannot skip forward.

---

## 3. The one design rule

**Colour carries meaning here, so it is never decoration.**

| Colour | Means | Used for |
| --- | --- | --- |
| Amber `#ffb224` | "this is the live, interactive thing" | the primary button, the current step, selection, the dimension lines, any number that changed |
| Green `#5fd38d` | **verified** | passed checks, the done badge, completed steps |
| Red `#ff5f56` | **stop** | refusals, failed checks, the cut-power button |

Nothing else may use green or red. The partition colours in section 4 were chosen to avoid both
hues for exactly this reason: a green partition block would read as "this one is safe".

---

## 4. Tokens

### 4.1 Colour

```
Ground and structure
  void        #0b0b0d   the window background
  deck        #141417   panels, cards, the rail, the readout
  deck-high   #1b1b20   the row the pointer is on, the current step, secondary buttons
  rule        #26262c   borders
  rule-soft   #1d1d22   dividers inside a panel

Text
  ink         #eae8e3   primary
  dim         #9a9892   secondary
  faint       #6a6862   labels, placeholders, disabled

Meaning
  signal      #ffb224   amber, the only interactive colour
  signal-dim  #b87b12   amber at rest (dimension lines)
  signal-ink  #1a1203   text on top of amber
  verified    #5fd38d   green
  stop        #ff5f56   red

Partition contents  (deliberately no green, no amber)
  ntfs        #5b6ee1   indigo
  ext4        #22b8c7   teal
  btrfs       #c86bd6   orchid
  xfs         #e0609a   rose
  swap        #a6896b   clay
  system      #4a4a54   graphite   (EFI system partition, BitLocker - both locked)
  unknown     #6f6d78   grey       (unrecognised contents)
```

A partition block is drawn in two tones:

- **Block background** = its content colour mixed with `void`: **22%** for ext4/btrfs/xfs,
  **26%** for ntfs/swap, **42%** for unknown. `system` is used at full strength, unmixed.
- **Used-space fill** = the content colour at full strength, drawn over the left part of the
  block (section 10.3).

### 4.2 Type

Two families, clearly different jobs:

- **Archivo** - all prose, headings, labels, buttons. Fallback: the platform UI sans.
- **JetBrains Mono** - every number, size, sector, device path, and the detail column of the
  checks list. Always with **tabular figures** so columns line up. Fallback: any mono.
- **Ubuntu Mono** - the recovery console only (section 12). It mirrors a real Linux console, so
  it keeps its own face on purpose.

| Role | Size | Weight | Tracking |
| --- | --- | --- | --- |
| Screen title (`h1`) | 1.7rem (25.5px) | 600 | -0.025em |
| Lede under the title | 0.92rem (14px) | 400 | normal |
| Readout section heading | 0.8rem (12px) | 600 | normal |
| Rail step | 0.88rem (13px) | 400, 600 when current | normal |
| Table row, checks row | 0.82rem (12px) | 400 (name 500) | normal |
| Button | 0.86rem (13px) | 500, 600 for primary | normal |
| Mono data | 0.76rem (11.5px) | 400 | normal |
| Micro label (capsules, scale, bar notes) | 0.64-0.72rem (10-11px) | 400 | normal |
| Big statistic | 1.5rem (22.5px) | 500 mono | -0.03em |

Sentence case everywhere. **No all-caps labels.** Body text never exceeds about 70 characters
per line: the lede is capped at 44rem (660px) and readout prose at the panel width.

### 4.3 Shape, spacing, motion

- **Radius:** 3px on buttons, inputs and the disk bar; 4px on panels; 50% on step circles,
  check marks and capsule dots; 999px (full pill) on sliver capsules.
- **Borders:** always 1px, `rule`. Panels additionally get a 1px top highlight of
  `#ffffff @ 3%` inset, which is what makes them read as lit from above rather than flat.
- **Spacing scale:** 4, 6, 8, 12, 16, 20, 24, 34px. Panel padding is 16-20px; row padding is
  8px vertical, 14px horizontal.
- **Motion** is for showing what changed, never for arrival flourish:
  - Screen change: fade in and rise 5px over 0.2s.
  - Hover and pressed states: 0.12s.
  - The running check pulses opacity 100% -> 35% -> 100% over 0.7s, looping.
  - All of it is disabled when the system asks for reduced motion.
- **Focus:** a 2px `signal` outline, offset 2px, on every focusable thing. It must be visible on
  the partition blocks too, since they are keyboard-operable.

---

## 5. The shell

Four regions, fixed to the window. The window never scrolls as a whole; only the work area and
the two panels scroll internally.

```
+-------------+------------------------------------+------------------+
|             |                                    |                  |
|    RAIL     |             WORK AREA              |     READOUT      |
|   202px     |         fills, scrolls             |      262px       |
|             |                                    |                  |
|             |                                    |                  |
|             +------------------------------------+------------------+
|             |  ACTION BAR                         status ... [buttons]|
+-------------+------------------------------------------------------- +
```

- Grid: `rail (202px) | work (1fr) | readout (262px)`, rows `1fr | auto`. The rail spans both
  rows; the action bar spans work + readout.
- Work area padding: 30px top, 34px sides, 22px bottom.
- Action bar padding: 13px vertical, 34px horizontal; 1px top border in `rule`; background is a
  vertical gradient from `deck` to `void`.
- Readout: 1px left border, `deck` background, 22px/19px padding.

**Vertical centring.** The work area centres its screen vertically when the screen is shorter
than the area, and falls back to top-aligned when it is taller. This is why there is no dead
space under the short screens. (CSS calls this `justify-content: safe center`; in a Rust
toolkit, compute the child height and only centre when it fits.)

### Responsive

- **Below 1180px wide:** the readout moves below the work area, full width, with a top border
  instead of a left border.
- **Below 820px wide:** the rail becomes a horizontal strip across the top. The step labels
  disappear, leaving only the numbered circles. The demo badge is hidden. Titles drop to
  1.35rem (20px). The window may scroll vertically as a whole at this size.
- At every size, nothing may overflow horizontally.

---

## 6. The rail

```
+-----------------------+
|  (o) BootShift        |   brand
+-----------------------+
|  (1) Arrange          |   current: amber circle, deck-high row
|   |                   |
|  (2) Plan             |   reached: dim text, clickable
|   |                   |
|  (3) Checks           |   unreached: faint text, not clickable
|   |                   |
|  (4) Restart          |
|   |                   |
|  (5) Done             |
|                       |
|      (flex spacer)    |
+-----------------------+
|  Disk                 |
|  [ /dev/demo 512.0 GiB v ]
|  [ Demo disk. Nothing |
|    here touches hardware. ]
+-----------------------+
```

**Brand.** A 17px circle, 1.5px border `#3a3a44`, `void` fill, with a 4px amber dot inset 1.5px
from its right edge: a platter with one sector lit. Next to it, "Boot" in `dim` and "Shift" in
`ink` at weight 700, 1.02rem (15px).

**Steps.** Each is a row of `[circle] [label]`, 8px gap, 7px/10px padding, 3px radius.

- Circle: 1.4rem (21px), 1px border, `void` fill, mono 0.68rem (10px).
- A 1px vertical hairline in `rule`, 7px tall, joins each step to the one above it, drawn
  5px above the row. This is what makes the rail read as a route rather than a menu.
- **Current:** row background `deck-high`, label `ink` weight 600, circle filled `signal` with
  `signal-ink` text.
- **Already passed:** circle border `#5fd38d @ 40%`, number in `verified`.
- **Reached but not current:** label `dim`, clickable, hover gives `deck-high` background and
  `ink` label.
- **Not yet reached:** label `faint`, disabled, no hover.

**Foot.** 1px top divider. Label "Disk" in `faint` 0.74rem (11px), then a select listing each
disk as `path` + two spaces + capacity, mono 0.76rem (11.5px). Below it, only in demo mode, a
badge: 1px border `#ffb224 @ 25%`, background `#ffb224 @ 6%`, amber text 0.72rem (11px),
reading "Demo disk. Nothing here touches hardware."

---

## 7. The action bar

Every screen's buttons live here, not inside the screen. Only the current screen's group is
shown.

Left: the status line, `dim` 0.82rem (12px), takes the remaining width. Right: the buttons,
10px apart, secondary first, primary last.

| Screen | Buttons (left to right) |
| --- | --- |
| arrange | `Start over` (secondary, disabled until something changed), `See the plan` (primary, same condition) |
| plan | `Adjust the layout`, `Run the safety checks` (primary, disabled unless the plan is doable) |
| checks | `Check again`, `Apply and restart later` (primary, disabled unless every check passed **and** the backup box is ticked) |
| restart | `Cut the power` (red, only while a demo run is playing), `Preview the restart` (primary, demo mode only, before the run starts) |
| done | `Plan another change` (primary) |

**Button styles.** Padding 7px/16px, 3px radius, 0.86rem (13px).

- Secondary: 1px `rule` border, `deck-high` background, `ink` text. Hover: border `#3a3a44`,
  background `#24242b`.
- Primary: `signal` border and background, `signal-ink` text, weight 600. Hover: `#ffc352`.
- Red: transparent background, `#ff5f56 @ 45%` border, `stop` text. Hover: `#ff5f56 @ 10%`
  background, solid `stop` border.
- Disabled (any kind): `deck` background, `rule-soft` border, `faint` text, default cursor.
  A disabled primary must **not** keep its amber fill; it drops to the same grey as the others.

---

## 8. The readout

The right panel. It is never empty and never decorative: it answers the question the current
screen raises. Content per screen is in section 9.

Two building blocks:

- **Heading:** 0.8rem (12px), weight 600, `dim`, 10px below it.
- **Fact row:** key on the left in `faint`, value right-aligned in mono `ink` 0.76rem (11.5px),
  5px vertical padding, 1px `rule-soft` divider below each except the last. A value that
  changed is `signal`; a value that is good news is `verified`.
- **Prose:** 0.8rem (12px), `dim`, line height 1.55, paragraphs 9px apart.
- **Partition title** (when one is selected): a 10px rounded swatch in that partition's colour,
  then its name at 0.95rem (14px) weight 600.
- **Used meter:** a 6px tall bar, `rule` background, 3px radius, filled from the left in
  `signal` to the used fraction.

Blocks are separated by 21px.

---

## 9. The screens

Each screen is: a header (title + lede), then its content. Buttons are in the action bar.

### 9.1 Arrange

Title: **"Arrange the disk"**
Lede: "Drag a partition to move it. Drag an edge to resize it. Nothing is written until you
apply."

Content, inside one panel (`deck`, 1px `rule`, 4px radius, 17px/19px padding):

```
Now                                        as the disk is today
[=================== the disk, read only ===================]

        <------- Windows recovery moves 83.5 GiB
        <-- Linux moves 83.5 GiB                              <- dimension gutter (sec 11)

After the restart                                drag to change
( EFI system partition 260.0 MiB ) ( Microsoft reserved 16.0 MiB ) ...   <- sliver capsules
[=================== the disk, editable ====================]
|   |   |   |   |   |   |   |   |   |   |   |   |   |   |   |            <- scale ticks
0                        238.0 GiB                    476.0 GiB
```

Then, below the panel, the partition table (section 12).

Bar captions are 0.78rem (12px) weight 600 `dim` on the left, with a mono 0.68rem (10px)
`faint` note on the right ("as the disk is today", "drag to change").

**Readout.** With nothing selected: heading "This disk", then facts - Device, Capacity,
Partition table, Partitions, Unallocated, Alignment ("1.0 MiB boundaries"). Then heading "Pick
a partition" and the line "Click a partition in the bar or the table to see what it holds and
how far it can go."

With a partition selected: its title and swatch, then Contents and Partition number; a "Space"
block with the used meter and Used / Size now / Size after / Smallest it can be; a "Position"
block with Starts now / Starts after / Data to copy, each as `sector 421,636,096` with thousand
separators. Values that differ from today are amber.

Status line while idle: "Drag a partition to move it, or an edge to resize it."

### 9.2 Plan

Title: **"The plan"**
Lede: "Every step the recovery system will take, in the order it takes them."

Content: a numbered list in one panel. Each row is `[circle] [sentence]`, 11px/15px padding,
1px `rule-soft` divider. The circle is 1.6rem (24px), 1px `rule` border, amber mono number.
**Numbering is correct here because this really is a sequence** - do not use numbered markers
anywhere else.

Step wording comes from the plan and must match the command line exactly:

```
Shrink partition 3 "Windows" to 116.5 GiB
Move partition 4 "Windows recovery" to 116.8 GiB (copies about 500.0 MiB)
Grow partition 5 "Linux" to 250.0 GiB
Set aside swap partition 6 (swap)
Recreate swap partition 6 (swap) at 460.0 GiB
```

A partition is named `partition <n> "<label>"` when it has a label, otherwise
`partition <n> (<contents>)`.

When there is nothing to do, the empty list shows: "Nothing to do yet. Move a partition on the
previous screen."

Below the list, any refusals (section 13).

**Readout.** Heading "What this costs": Steps, Data copied, Chunk size ("64 MiB"), Restarts
("1"), Rough time (`Nm Ss`, estimated at 450 MiB/s - this is a rough figure for the user, not a
promise). Then heading "What protects you" with three sentences:

> Data is copied and verified in its new place before the partition table is touched.
>
> A chunk is recorded only after the drive confirms the write, and only one write is in flight
> at a time.
>
> The journal on the EFI partition is the record of truth, so a power cut resumes from the last
> verified chunk.

### 9.3 Checks

Title: **"Safety checks"**
Lede: "All of them pass, or BootShift writes nothing at all."

Content: the checks list in one panel, then the gate row.

**Checks list.** One row per check: `[mark] [name] [detail]` on a grid of
`1.1rem | minmax(9rem, 0.9fr) | 1.6fr`, 8px/15px padding, 1px `rule-soft` dividers. The name is
prose; the detail is mono 0.74rem (11px).

| Row state | Mark | Name | Detail |
| --- | --- | --- | --- |
| waiting | empty 1.1rem circle, 1px `rule` border | `faint` | blank |
| running | circle with an amber centre dot, pulsing | `faint` | blank |
| passed | green circle, `#5fd38d @ 12%` fill, `✓` | `ink` | `dim` |
| failed | red circle, `#ff5f56 @ 12%` fill, `✕` | `ink` | `stop` |

Rows resolve **one at a time, 110ms apart**, top to bottom. This is the only orchestrated
animation in the app and it is worth keeping: it shows the checks being done rather than
asserting that they were.

The twelve checks, in this order, with the demo's details. **The names mirror
`engine::preflight::evaluate` - if that function changes, change these too:**

| Name | Demo detail |
| --- | --- |
| Firmware mode | UEFI, Secure Boot on |
| Partition table | GPT, 6 partitions, backup header intact |
| How the disk is attached | NVMe, direct (not over USB) |
| Power | On mains, battery at 87% |
| Disk health | SMART self-assessment passed |
| Kernel modules | Present for 6.11.0-9-generic |
| Room on the EFI partition | 180 MiB free, 46 MiB needed |
| Bootloader | GRUB (ubuntu), signed shim in place |
| Hibernation | No hibernated system in swap |
| Encryption | No BitLocker volume on this disk |
| Filesystems | ext4 and NTFS both clean and readable |
| Partition-table backup | Written to the EFI partition first |

**Gate row.** One panel below the list, contents spread apart:

- Left: a checkbox (1.05rem / 16px, amber accent) and the label "My important files are backed
  up somewhere off this disk". The checkbox and its text are **vertically centred on each
  other**; the whole row is one click target.
- Right: a `Save the partition table` button. It writes a readable copy of the current layout
  to a file the user picks. In demo mode it still produces a real file, with a note inside
  saying the data is made up.

Status line when everything passes: "All green. Boringly green. Confirm your backup to unlock
apply." When something fails: "Apply stays locked until every check passes."

**Readout.** Heading "Checks": Passed (`12 of 12`, green when all passed), Verdict ("running",
"safe to start" or "refused"). Then "What applying does":

> BootShift copies its recovery system onto the EFI partition and points the next boot at it.
> Your disk is not touched yet.
>
> The work happens during the restart you choose, before any operating system starts.

### 9.4 Restart

Title: **"During the restart"**
Lede: "This is what the computer shows while BootShift works, before any operating system
starts."

Content: the recovery console (section 14), left-aligned with the title, capped at 58rem
(870px) wide.

**Readout.** Heading "If the power goes out":

> Turn the computer back on. BootShift reads its journal, finds the last chunk the drive
> confirmed, and carries on from there.
>
> A half-written chunk is simply redone: a chunk is never larger than the distance it moves, so
> it can never overwrite its own source.

Then heading "What never happens" with three facts: Table before data - "never"; Unverified
chunk - "never journaled"; Boot into a half-moved disk - "blocked".

Status line: in demo mode before the run, "Demo: preview what the restart looks like, power cut
included."; during the run, "Copying. Pull the plug whenever you like."; outside demo mode,
"Everything is ready. Restart the computer to begin."

### 9.5 Done

Title: a 2rem (30px) green circle with `✓` beside the word **"Done"**.
Lede (demo): "Every moved byte was read back and verified. The power cut in the middle changed
nothing."
Lede (real): "The disk matches the plan. Every moved byte was read back and verified."

Content:

1. A row of four statistics, equal columns, 1px `rule` grid lines, `deck` cells, 15px/16px
   padding. Value in mono 1.5rem (22.5px) weight 500; label below in `faint` 0.76rem (11.5px).
   The four are: data moved, chunks verified, power cuts survived (singular "power cut
   survived" when it is 1), and total time (labelled "simulated time" in demo mode).
2. A read-only disk bar of the **final** layout, in a panel captioned "Your disk now" with the
   note "verified byte for byte". Showing the result back to the user is the point of this
   screen; do not drop it.
3. The note: "The full log and your old partition table stay on the EFI system partition until
   the next change."

**Readout.** Heading "Kept on the EFI partition": Partition table backup - "saved"; Move journal
- "saved"; Recovery boot redirect - "removed". Then "If something looks wrong" with the line
"Undo runs the same plan in reverse while the freed space is still empty." (prefixed "In the
real app," in demo mode).

---

## 10. The disk bar

This is the hero of the interface. Everything else stays quiet so this can be loud.

### 10.1 Geometry

The bar represents the disk's **usable region** only (`geometry.usable`, a start sector and a
length in sectors). A partition occupying sectors `[start, start+len)` is drawn at:

```
left   = (start - usable.start) / usable.len  * bar_width
width  = len                      / usable.len * bar_width
```

Nothing else is positional. Do not add gaps between blocks: adjacency on screen means adjacency
on the disk, and a gap means free space.

### 10.2 The bar itself

- Read-only bar: 3.25rem (49px) tall. Editable bar: 4.35rem (65px) tall.
- 1px border `rule` (editable: `#32323a`), 3px radius, clipped contents.
- Background: `#08080a` with 1px vertical hairlines of `#ffffff @ 2.2%` every 7px. This is the
  unallocated texture, so free space reads as empty rather than black.

### 10.3 A partition block

```
+--------------------------------------------------+
|[|] Windows                                    [|] |   <- edge handles
|[|] 116.5 GiB            : :                   [|] |   <- move grip (centre)
|[|]#############                               [|] |   <- used fill behind the text
+--------------------------------------------------+
```

- Padding 6px/8px. 1px right border in `#08080a` so neighbours stay distinguishable.
- Minimum width 6px.
- **Used fill:** an overlay pinned to the left of the block, width
  `min(100%, used / len * 100%)`, painted in the partition's full-strength colour with 1px
  vertical hairlines of `#08080a @ 35%` every 6px. The remainder of the block shows the mixed
  (darker) tone, which is that partition's free space.
- **Name:** 0.74rem (11px) weight 600, white `#fbfaf8`, with a 1px text shadow so it stays
  readable over the fill. Falls back to the contents name when the partition has no label.
- **Size:** mono 0.66rem (10px), 88% opacity, below the name. Always the size *after* the
  current edit.
- **Locked** partitions (EFI system, BitLocker) get a 45-degree hatch of `#000000 @ 32%` and a
  not-allowed cursor. They cannot be moved, resized or selected for editing.
- **Selected** gets a 1px inset amber ring plus a 1px amber outer ring, and is raised above its
  neighbours.
- Hover on a movable block raises its brightness 14%.

### 10.4 Drag affordances

These are what the user looks for first, so they are visible **at rest**, not only on hover.

- **Edge handles.** One at each end, width `min(0.7rem, 20%)` of the block - the 20% cap is
  what keeps a narrow partition from being nothing but handles, so the middle is always
  grabbable for a move. Each handle has: a dark scrim `#040406 @ 30%`; a 2px vertical line in
  `#ffffff @ 32%` flush with the outer edge; and **two 1px white vertical lines 1.05rem (16px)
  tall centred in the handle, 3px apart**. That pair of lines is the universal "grab this edge"
  mark and is the single most important affordance on the screen. On hover the scrim becomes
  `#ffb224 @ 25%`, the edge line becomes amber, and the two lines go pure white. Cursor:
  horizontal resize.
- **Move grip.** A 2x3 field of 1px white dots at the centre of the block, 6px by 5px apart,
  `min(0.75rem, 40%)` wide, drawn at 40% opacity at rest and 95% on hover or focus. It does not
  take pointer events; it is a sign, not a target. Cursor over the block: open hand, closing
  while dragging.
- While a drag is in progress the **whole bar** keeps the matching cursor, so the pointer does
  not flicker when it crosses a neighbour.

### 10.5 Slivers

A partition narrower than **1% of the usable region** is a sliver: on a 476 GiB disk that is
anything under about 4.8 GiB, which in practice means the EFI system partition, Microsoft
reserved, and the Windows recovery partition.

A sliver:

- is forced to a minimum width of **9px** and raised above its neighbours, because otherwise a
  neighbour's 8px-wide resize handle sits on top of it and it cannot be clicked at all;
- drops its label, its size text and its move grip - there is no room;
- drops its edge handles entirely, since two handles would cover the whole block. It can still
  be **moved** by dragging, and resized with the keyboard;
- gets a 1px inset ring of `#08080a @ 85%` so two adjacent slivers still read as two things;
- gets a **callout capsule** above the bar instead of a label inside it.

### 10.6 Sliver capsules

Above the editable bar only (the read-only bar does not repeat them), in a strip 1.95rem (29px)
tall:

```
( • EFI system partition 260.0 MiB )  ( • Microsoft reserved 16.0 MiB )  ...
   \                                     |
    \_____________________________       |
                                  \      |
[|][==============================][sliver][=================]
```

- Capsule: fully rounded pill, 1px `rule` border, `deck-high` background, mono 0.64rem (10px),
  padding 1px/8px, max width 14rem (210px) with the name ellipsised if it overruns. Inside:
  a 6px dot in the partition's colour, the name in `ink`, the size in `faint`.
- Hover (when the partition is movable): border and text turn amber, cursor pointer. Clicking
  one focuses and selects that sliver, which is how a keyboard user reaches it.
- **Placement.** Walk the slivers left to right. For each, the preferred position is centred on
  its block; clamp it so it starts no earlier than the previous capsule's right edge **+6px**
  and ends no later than the bar's right edge. This spreads capsules apart when two slivers sit
  almost on top of each other, which is the normal case at the start of a disk.
- **Leader lines.** An 8px tall strip under the capsules holds one 1px line per sliver, stroke
  `#3b3b45`, from the capsule's horizontal centre at the top to the sliver's centre at the
  bottom. The slant is what ties a displaced capsule back to its partition.
- Recompute the whole strip whenever the bar's width changes.

### 10.7 The scale

Under the editable bar only: a 5px tall tick strip with a major 1px `dim` tick every 10% and a
minor 1px `rule` tick every 2.5%; below it, three mono 0.64rem (10px) `faint` labels spread
across the width - `0`, the half-capacity figure, and the full capacity.

---

## 11. The dimension gutter

Between the "Now" bar and the "After" bar. This is the one piece of visual invention in the
design and it is what makes the screen legible at a glance, so build it.

Every partition **whose start sector changed** gets one line, in an engineering-drawing idiom:

```
   |<------------------------------- 1px line, amber-dim
   ^    Windows recovery moves 83.5 GiB       <- label, amber, sits above the line
   arrowhead at the destination end
```

- The line spans **exactly the distance travelled**: from the old start sector to the new start
  sector, in the same percentage space as the bars. Not the union of the two extents - the line
  *is* the measurement, and it must agree with its label.
- 1px high, colour `signal-dim` `#b87b12`.
- A 7px tall, 1px wide tick at each end, centred on the line.
- A solid triangle 7px long and 9px tall in full `signal` at the **destination** end, pointing
  the way the partition travels.
- Label: mono 0.66rem (10px) in `signal`, centred on the line, sitting just above it, with a
  `deck` background behind it so it punches a hole in the line. Text is
  `<partition name> moves <distance>`.
- **One line per moved partition, each on its own row**, 1.5rem (22.5px) apart, the first row
  1.45rem (22px) down from the top of the gutter. Stacking rather than overlapping is how real
  dimension drawings do it, and it means lines never collide. The gutter's height is
  `moves * 22.5px + 16px`.
- When nothing has moved the gutter collapses to a 0.9rem (13px) strip with a 1px dashed
  `rule-soft` line along the bottom, which keeps the two bars visually separated.

---

## 12. The partition table

Under the disk on the arrange screen. Slivers are unreadable inside the bar, so this is where
every partition can actually be read and compared.

One panel, 1px `rule`, 4px radius, clipped. Columns:

```
[sw] Partition            Contents     Size        Used         Change
 ::  EFI system partition  EFI system   260.0 MiB   40.0 MiB     locked
 ::  Microsoft reserved    unrecognised  16.0 MiB   16.0 MiB     unchanged
 ##  Windows               NTFS         116.5 GiB   60.0 GiB     -83.5 GiB
 ##  Windows recovery      NTFS         800.0 MiB  500.0 MiB     moves
 ##  Linux                 ext4         250.0 GiB   90.0 GiB     moves
 ##  swap                  swap           8.0 GiB   empty        unchanged
```

- Grid: `11px | minmax(105px, 1.7fr) | 105px | minmax(75px, 1fr) | minmax(90px, 1fr) | 135px`,
  15px gaps, 8px/14px padding, 1px `rule-soft` divider under each row.
- Header row: `deck-high` background, `faint` text at 0.72rem (11px). Its columns must line up
  with the body rows - give it the same grid, not an approximation.
- Swatch: 10px square, 2px radius, the partition's full-strength colour.
- Name in `ink` weight 500; contents, size and used in mono `dim` 0.76rem (11.5px); "empty"
  instead of a figure when nothing is used.
- **Change column**, right-aligned, mono 0.74rem (11px): `locked` or `unchanged` in `faint`
  when nothing happens to it; otherwise amber, listing the size delta (`+12.0 GiB` /
  `-83.5 GiB`) and `moves`, comma-separated, whichever apply.
- Each body row is a button. Hover gives `deck-high`. The selected row gets a 2px amber left
  edge and a `#ffb224 @ 6%` wash.
- **Selection is shared.** Picking a row selects that partition in both bars and fills the
  readout; picking a block in a bar highlights the row. Clicking the selected one again clears
  the selection.

---

## 13. Refusals

A refusal is the app saying no, and it always carries both a reason and a fix. Render them as a
list under whatever screen produced them.

Each: 1px `#ff5f56 @ 30%` border with a 2px solid `stop` left edge, `#ff5f56 @ 5%` background,
3px radius, 11px/14px padding, 8px apart. The reason in 0.86rem (13px) `ink`; the fix below it
in 0.8rem (12px) `dim`, prefixed with the word **"Fix: "** in `ink` weight 600.

Never show a refusal without its fix, and never soften the reason. "Windows is hibernated" with
"Turn off Fast Startup and shut Windows down fully" is the model.

---

## 14. The recovery console

A faithful drawing of the screen the machine shows during the restart. It is deliberately
**off the app's palette**, because it is a real Linux text console, not part of the app.

- Background `#040505`, 1px `rule` border, 4px radius, 22px/27px padding, minimum height 15rem
  (225px), a very faint green inner glow (`#3fe081 @ 3%`, 90px blur, inset).
- Ubuntu Mono, 1rem (15px), line height 1.7.
- Colours: body `#a9b6ad`, dim `#566157`, bright `#eef3ee` bold, green `#3fe081`,
  amber `#e0a33e`.
- Horizontal rules between sections: 1px `#141a15`.

Layout while running:

```
BootShift recovery                                        <- bright
moving your partitions, before any operating system starts <- dim
------------------------------------------------------------
████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░  11%                  <- green/dim, 36 cells, % bright
10.3 GiB of 90.5 GiB moved
chunk 165 of 1448 copied, flushed, verified, journaled     <- dim
------------------------------------------------------------
Power cut? Rude, but fine. Turn the computer back on.      <- green
BootShift resumes from the last verified chunk. No drama.  <- green
Do not boot Windows from the firmware menu. A half-moved disk  <- amber
is not something it handles gracefully.                    <- amber
```

When it finishes, the last block becomes a single green line: "Done. Restarting into your
system.", and the chunk line reads "all 1448 chunks copied, flushed, verified, journaled".

### The demo run

Chunk size is 64 MiB, so `total_chunks = ceil(copy_bytes / 64 MiB)`, minimum 1. The run ticks
every **90ms**, advancing `max(1, round(total_chunks / 280))` chunks per tick, so any plan
finishes in roughly 25 seconds regardless of size.

**The power cut** is the demo's whole argument, so get the timing right:

1. The screen goes completely black for **900ms**.
2. `power restored. firmware hands over to BootShift recovery.` (dim), wait 650ms.
3. `journal: plan found. last verified chunk N of M.` (dim), wait 650ms.
4. `that was rude. resuming from chunk N.` (green), wait 1100ms.
5. The full console reappears at chunk N and the run continues.

`N` is the chunk the run had reached, and nothing is lost. Count the cuts; the done screen
reports them.

**Known trap.** A plan that only moves *empty* partitions copies zero bytes, so the run
finishes in one tick and the cut-power button never appears. The demo story that works is:
shrink Windows, slide the Windows recovery sliver left, then slide Linux left - about 90 GiB
copied, a 25-second run with room to pull the plug.

---

## 15. Interaction rules

These are a product decision and are already mirrored in `core::validate`. **Do not change the
semantics when porting; only the rendering.**

| Input | Effect |
| --- | --- |
| Drag the body of a partition | moves it, keeping its length |
| Drag its left edge | moves its start, keeping its end |
| Drag its right edge | moves its end, keeping its start |
| Left/Right arrow (block focused) | moves it by 1 GiB |
| Shift + Left/Right | moves its **end** by 1 GiB, i.e. resizes |
| Click a block, capsule or table row | selects it |

Every resulting position is then **snapped and clamped**, in this order:

1. Snap the requested sector to the disk's alignment (1 MiB), rounding to nearest. Clamping
   bounds are aligned too: lower bounds round up, upper bounds round down.
2. Clamp between neighbours: a partition may use the space from the end of the partition before
   it to the start of the partition after it, in the *target* layout, bounded by the usable
   region.
3. Clamp the length: never below the partition's minimum, never above its maximum.
   - Minimum: for a shrinkable filesystem (ext4, NTFS, Btrfs, swap), the filesystem's own
     minimum rounded up to alignment; swap may go down to a single alignment unit; anything
     else cannot shrink at all, so its minimum is its current length.
   - Maximum: unbounded for a growable filesystem (ext4, NTFS, Btrfs, XFS, swap); otherwise the
     current length.
4. If the clamps leave no valid position, make no change at all. A drag that cannot move must
   be a no-op, never a jump.

Movability: **the EFI system partition and any BitLocker volume can never be moved or
resized.** Everything else can be moved; whether it can be *resized* depends on its contents as
above.

After any change, wait **300ms** of quiet before asking the backend to re-plan, so a drag does
not fire a request per pixel.

---

## 16. The backend contract

The GUI never touches a disk. It calls five commands and renders whatever comes back. Every
reply is one of:

```json
{ "result": "ok", "value": <T> }
{ "result": "refused", "refusals": [ { "reason": "...", "fix": "..." } ] }
```

| Command | Arguments | Value on success |
| --- | --- | --- |
| `list_disks` | - | `[ DiskSummary ]` |
| `probe` | `disk` | `Probed` |
| `plan` | `disk`, `target` | `Planned` |
| `check` | `disk`, `target` | `[ CheckResult ]` |
| `apply` | `disk`, `target` | `{ "restart_to_begin": true }` |

```
DiskSummary  { path, size, transport, removable, partition_table }
Probed       { disk, size, layout, target }
Layout       { geometry, partitions }
Geometry     { sector_size, usable: { start, len }, alignment }     // sectors
Partition    { number, unique_guid, type_guid, name, attributes,
               extent: { start, len }, content, used, min_len }
Target       { "<unique_guid>": { start, len }, ... }               // where each should end up
Planned      { operations, copy_bytes, unavailable: [Refusal] }
Operation    Shrink { partition, len } | Grow { partition, len }
             | Move { partition, from, to, len, copy_estimate }
             | RemoveSwap { partition } | RecreateSwap { partition }
CheckResult  { name, detail, passed }
Content      EfiSystem | Ext4 | Ntfs | Btrfs | Xfs | Swap | BitLocker | Opaque
```

All positions and lengths are **sectors**, not bytes. Convert with `geometry.sector_size`, and
do it in one place.

`unavailable` is the list of operations this build cannot yet perform safely; while it is
non-empty the plan cannot proceed, and its entries are shown as refusals.

---

## 17. The demo disk

The browser build talks to a made-up backend so the whole flow can be walked through without
hardware, and so the judges' demo never depends on a real disk. Reproduce it exactly: the
screenshots and the timings above assume these numbers.

512 GB disk, 512-byte sectors, 1 MiB alignment, usable region 476 GiB starting at 1 MiB.

| # | Name | Contents | Size | Used | Minimum |
| --- | --- | --- | --- | --- | --- |
| 1 | EFI system partition | EfiSystem | 260 MiB | 40 MiB | - |
| 2 | Microsoft reserved | Opaque | 16 MiB | 16 MiB | - |
| 3 | Windows | Ntfs | 200 GiB | 60 GiB | 61 GiB |
| 4 | Windows recovery | Ntfs | 800 MiB | 500 MiB | 600 MiB |
| 5 | Linux | Ext4 | 250 GiB | 90 GiB | 92 GiB |
| 6 | (no name) | Swap | 8 GiB | 0 | 0 |

They are laid out back to back from 1 MiB, leaving about 17 GiB unallocated at the end. The
demo's `plan` reports a Shrink for any partition that got smaller, a Move for any that changed
start (copying `min(used, len)`), and a Grow for any that got bigger. `check` returns the twelve
rows in section 9.3, all passing. `apply` succeeds without doing anything.

---

## 18. Done means

Before calling the port finished:

- [ ] The whole flow runs start to finish without touching a disk: drag, plan, checks, apply,
      preview the restart, cut the power, watch it resume, read the done stats.
- [ ] A partition can be moved by dragging its middle and resized by dragging either edge, and
      the edge handles are visibly marked at rest.
- [ ] The Windows recovery sliver can be clicked, selected and dragged, and its capsule above
      the bar names it.
- [ ] Moving something draws a dimension line whose label matches the distance the partition
      actually travelled.
- [ ] Every control is reachable with the keyboard, in a sensible order, with a visible focus
      ring - including the partition blocks, which respond to the arrow keys.
- [ ] Nothing overflows horizontally at 1600px, 1180px, 1100px or 780px wide.
- [ ] Green appears only on verified things and red only on refusals.
- [ ] A disabled primary button does not look amber.
- [ ] The clamping rules in section 15 behave identically to `core::validate`, including the
      case where a drag is impossible and must do nothing.

---

## 19. Ownership while two sessions are running

The reference GUI session owns `gui/`, this file, and `task.md`'s Phase 13 lines. Another
session owns `crates/`, `xtask/`, `issues.md` and the other docs. Both share one working tree,
so commit only your own paths and never run `git commit`/`push` without being asked.

**Bug worth not reintroducing:** a static `import { invoke } from "@tauri-apps/api/core"` used
to crash the whole dev page when the package could not be resolved, leaving nothing clickable.
The web build calls `window.__TAURI_INTERNALS__` directly instead. If you port the shell, keep
the equivalent guard: the demo path must work even when the host bridge is absent.
