# Demo script

Three acts, about four minutes. Acts 1 and 2 run anywhere; act 3 needs the Linux box.

## Act 1 — the problem (30 s)

"Every dual-booter knows this: Windows has 200 GB free, Linux is full, and moving that space
means a live USB, GParted, an afternoon, and a prayer against power cuts. We built the tool we
wished existed: drag the partitions, restart once, and a power cut can't hurt you."

## Act 2 — the product (90 s, browser)

```
cd gui && npm ci && npm run dev    # http://localhost:1420
```

1. **Arrange**: drag the Windows edge left, drag Linux bigger. Point out used vs free space
   in the bar, and that nothing touches the disk.
2. **Plan**: read the steps aloud; they're honest about what gets copied and that the
   "unmovable" recovery partition moves.
3. **Checks**: every refusal has a fix. Apply stays locked until the list is boringly green.
4. **Restart preview**: start it, then press "⏻ Cut the power" and let the room watch it
   resume from the last verified chunk.

Say clearly: "this screen is a demo backend; what you see next is the real engine."

## Act 3 — the proof (2 min, real engine in a VM)

```
cargo xtask boot-proof
```

Narrate while it runs: a real Ubuntu VM with UEFI firmware, its own disk as NVMe. The real
`bootshift apply` runs inside it: safety checks, recovery system onto the EFI partition, boot
order changed. The VM restarts into recovery, starts moving a partition with 1.2 GB of
checksummed files, and **xtask kills the VM mid-move: that is a power cut**. It powers back
on, recovery reads the journal, resumes from the last verified chunk, finishes, restores
normal booting, and the VM comes back up. The script then verifies every file hash, the
partition table, and the restored boot configuration, and prints:

```
BOOT PROOF PASSED
  moved and grew the data partition on the guest's own disk
  survived a power cut mid-move and resumed from the journal
  every file byte-identical, table on target, booting restored
```

End on: "The journal design means the progress bar never claims more than what would survive
a power cut at that instant. We didn't make the demo safe; we made the product safe and then
demoed it."

## If asked

- **Windows?** Moving NTFS works the same way (boot sectors fixed in the move step); proving
  Windows 11 *boots* afterwards is our next validation gate (task 0.2), deliberately listed
  as an open risk rather than hand-waved.
- **Shrinking ext4/NTFS?** Needs our crash-safe "guarded run" wrapper around resize tools;
  the undo-log core is built and crash-tested, the kernel transport is in progress (task 0.6).
- **Why not GParted?** GParted is the engine room; it needs a live USB and dies with the
  power. BootShift is the layer that plans, survives, and never needs external media.
