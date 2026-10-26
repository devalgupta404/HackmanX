# Intent-first interface

Direction for the desktop app: open with a person's **goal**, not the disk editor.

Full mockups (one per goal, in the app's skin):
<https://claude.ai/code/artifact/7aeed6d9-a66a-40a7-ad3d-496fa29e0763>

## The problem

The app opens on the disk editor — a bar of partitions to drag, with starts and sizes.
That is right for someone who already thinks in partitions. Most people don't. They want to
*install an OS* or *make a drive bigger*, not *move partition 3 left by 40 GB*. The engine
already does the hard thinking; the gap is the front door.

## The idea

A **home screen of goals**. Each goal opens a small, single-purpose screen that asks only what
that goal needs, then hands the same `Target` to the existing plan → checks → restart flow. The
disk editor becomes one goal — **"Do it myself"** — kept first-class for power users.

Flow: `Home → Goal screen → Plan → Checks → Restart → Done`
(the "Do it myself" card routes to today's editor instead of a goal screen, then rejoins.)

## The goals

| Card | What it asks | Status of the underlying op |
|---|---|---|
| **Install an operating system** (hero) | How much room for the new OS | Free a contiguous block — move/shrink; the space-finder is the one new bit of logic |
| **Give a partition more space** | Which one is full, how much bigger | Grow + shrink a neighbour |
| **Free up space from a partition** | Which has room, how much to free | Shrink — Btrfs/swap ship today; ext4/NTFS wait on guarded runs |
| **Move a partition** | Which one, where | Move (works today) |
| **Clean up a drive** | Which to remove, delete or reformat | Remove/recreate; destructive, so a red warning and a deliberate confirm |
| **Do it myself** (advanced) | — | Today's editor, untouched |

## What's reused vs new

- **Reused:** the engine, planner, preflight checks, recovery system, the disk bar and editing
  logic, and the Plan / Checks / Restart / Done screens. A goal screen's only job is to produce
  a `Target`.
- **New:** the Home screen (a thin menu), one small screen per goal, and the "Install an OS"
  space-finder.

## Principles to hold

1. Every goal ends at the same disk-bar preview and the same safety checks — simple means fewer
   questions, never less honesty about what will happen.
2. Keep "Do it myself" a visible card, not a buried menu: it is the product's credibility.
3. Lead with "Install an OS" — the most common reason a layman touches partitions, and the
   clearest demo story.
4. Don't offer a goal the engine can't yet honour. If a goal needs the guarded-run shrink that
   isn't shipped, say so on that screen rather than failing at Apply.
5. One accent, one meaning: amber is "do this", green is verified, red is stop. Goal cards take
   variety from the cool partition hues, never a second accent.
