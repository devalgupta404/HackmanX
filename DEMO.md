# BootShift demo

Everything below runs the real engine on a real disk. There is no mock: a throwaway Ubuntu VM
boots with UEFI firmware, its own SATA disk, and the same static `bootshift` binary that would
ship. The only thing fake about it is that the disk is a file, so a power cut is just us killing
QEMU.

You have three ways to show it, from most hands-on to most hands-off:

| Command | What it proves | Who drives |
| --- | --- | --- |
| `cargo xtask demo` | The CLI moving, growing and shrinking a live partition, then a power cut resuming | You, by hand, over SSH |
| `cargo xtask demo-gui` | The real desktop GUI driving the VM's disk over a forwarded bus | You, by dragging |
| `cargo xtask boot-proof` | The whole thing, unattended, with the power cut scripted | Nobody, it runs itself |

All three reuse one prepared image: a 12 GiB disk whose data partition is **7 GiB of Btrfs holding
about 400 MiB of checksummed random files**, with a free gap in front of it and a couple of GiB of
free space behind it. The slack is the point. A partition sized tight to its data can only sit
still. This one can move, grow into the space behind it, and shrink to give space back, and every
file is verified by hash before and after. The move copies roughly 700 MiB of Btrfs extents, so its
progress bar runs for about twenty-five seconds on this box: long enough to watch and narrate, long
enough to cut the power in the middle.

Why Btrfs: it is the one filesystem this build can move, grow **and** shrink today, because its
online resize is journaled by the kernel and survives a power cut on its own. ext4 and NTFS move
and grow, but shrinking them safely needs the guarded-run wrapper that is not shipped yet (see the
caveats at the end). So the demo uses Btrfs to tell the whole story in one sitting.

---

## Act 1, the pitch (30 s, no machine)

"Every dual-booter knows the bind. Windows has 200 GB free, Linux is full, and reclaiming that
space means a live USB, GParted, an afternoon, and a silent prayer that the power holds. We built
the tool we wanted: drag the partitions, restart once, done. And if the power dies in the middle,
you lose nothing. Not 'probably nothing'. Nothing. Let me show you the engine, not a slide."

---

## Act 2, the hand-driven CLI (`cargo xtask demo`)

On the Linux box:

```bash
cargo xtask demo
```

A VM window opens and stays open. Keep it on screen: the recovery console appears there during the
restart. The command prints an SSH line and the path to a ready plan (`/home/ubuntu/move-left.json`,
which moves the data partition left into the gap and grows it to fill the space). In a second
terminal, connect to the VM:

```bash
ssh -p <PORT> -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ubuntu@127.0.0.1
```

### 2a. Before: read the disk

```bash
sudo bootshift probe /dev/sda
```

Point at the data partition: 7 GiB, Btrfs, sitting after a gap, with free space behind it. Then
show that BootShift is honest about what it is going to do before it touches a byte:

```bash
sudo bootshift plan /dev/sda /home/ubuntu/move-left.json
```

Read the steps aloud. It copies only the used ranges, moves data before it ever rewrites the table,
and grows the filesystem last. Then run the safety checks, which change nothing:

```bash
sudo bootshift check /dev/sda /home/ubuntu/move-left.json
```

"Apply stays locked until this list is boringly green. Every refusal carries a fix in plain words,
not a code."

### 2b. Apply, then watch the move

```bash
sudo env BOOTSHIFT_EXTRA_KERNEL_ARGS='console=tty0 console=ttyS0' \
     bootshift apply /dev/sda /home/ubuntu/move-left.json --yes
sudo reboot
```

`apply` runs the checks again, writes the recovery system onto the EFI partition, and changes the
boot order so the next restart lands in recovery instead of Ubuntu. Nothing on the data partition
has moved yet: data first, table last.

Now watch the VM window. The machine reboots into the BootShift recovery console and starts moving
the partition. Because there is roughly 700 MiB of real data to copy, **the progress bar climbs for
about twenty-five seconds, not a blink**. Narrate it: "every chunk it marks done is a chunk already
verified in its new home. The bar can't lie about progress a power cut would undo." When it finishes
it grows the Btrfs to fill the new, larger partition, restores the normal boot order, and reboots
into Ubuntu.

### 2c. After: prove nothing broke

Reconnect once it reaches the login screen, and show the move landed and every file survived:

```bash
sudo bootshift probe /dev/sda
sudo mount /dev/sda2 /mnt && cd /mnt && sudo sha256sum --quiet -c MANIFEST && echo ALL-FILES-OK
```

The data partition now starts earlier and is larger, and every checksum matches.

### 2d. The money shot: cut the power

This is the part that wins the room. Start the move again (apply a plan and `sudo reboot`), and
while the progress bar is climbing, **close the VM window**. That is a power cut at the worst
possible instant: mid-copy, table half-considered. Then, from the box, bring it back:

```bash
cargo xtask demo        # no --fresh: it reuses the same disk
```

The firmware boot order that `apply` set is still in the kept variables file, so the machine boots
straight back into recovery. It reads the journal, sees the last chunk it had durably verified, and
resumes from exactly there. It finishes, restores booting, and comes up clean. Mount and check the
MANIFEST again: all files intact.

"We did not make the demo safe. We made the product safe, and then demoed it."

### 2e. Optional: grow and shrink to show the slack

The headline plan moves and grows. To tell the fuller story, shrink the Btrfs partition to give
space back, which this build can do today, then grow it again. Build each target from the live
layout so the numbers are always right:

```bash
DATA=/dev/sda2
PARTUUID=$(sudo blkid -s PARTUUID -o value $DATA)

# Shrink: give back 2 GiB (2 * 1024 * 2048 sectors). Safe, far more than the data needs.
sudo bootshift probe /dev/sda --target-template \
 | python3 -c "import sys,json; t=json.load(sys.stdin); t['$PARTUUID']['len']-=2*1024*2048; json.dump(t,sys.stdout,indent=2)" \
 > /home/ubuntu/shrink.json
sudo env BOOTSHIFT_EXTRA_KERNEL_ARGS='console=tty0 console=ttyS0' \
     bootshift apply /dev/sda /home/ubuntu/shrink.json --yes
sudo reboot
```

After it reboots, `sudo bootshift probe /dev/sda` shows the partition smaller and 2 GiB now free
behind it, with the MANIFEST still clean. To grow it back, build the same way with
`['len']+=2*1024*2048` and apply again. Three operations, one filesystem, every file verified each
time.

Stop the VM when done: `pkill -f demo.qcow2`.

---

## Act 3, the real GUI (`cargo xtask demo-gui`)

Same disk, same engine, but now the actual desktop app drives it.

```bash
cargo xtask demo-gui
```

This boots the VM, starts `bootshiftd` (the root helper) inside it, and forwards the VM's system
D-Bus socket to this box. It prints the exact launch line, which looks like:

```bash
cargo build -p bootshift-gui --release
DBUS_SYSTEM_BUS_ADDRESS=unix:path=/tmp/bootshift-vm-bus ./target/release/bootshift-gui
```

The GUI you open is talking to the VM's **real** disk over that bus. Drag the data partition's edge
to move and resize it, watch the disk bar show used versus free, run the checks, and apply. When it
says to restart, reboot the VM (the command is printed too) and watch the recovery console come up
in the VM window. Reopen the GUI afterward to see the new layout.

Stop everything: `pkill -f demo-gui.qcow2; pkill -f /tmp/bootshift-vm-bus`.

---

## Act 4, the unattended proof (`cargo xtask boot-proof`)

If you only run one command, run this one.

```bash
cargo xtask boot-proof
```

Narrate while it works. It boots the VM, applies a plan that moves the data partition left and grows
it, restarts into recovery, and starts copying the Btrfs used ranges. Partway through, between 5%
and 90%, **the harness kills QEMU: that is the power cut.** It powers back on, recovery reads the
journal, resumes from the last verified chunk, finishes, and restores normal booting. Then the
script re-mounts the partition and checks every file hash, confirms the table is exactly on target,
confirms the GRUB redirect was removed and the boot order restored, and prints:

```
BOOT PROOF PASSED
  moved and grew the data partition on the guest's own disk
  survived a power cut mid-move and resumed from the journal
  every file byte-identical, table on target, booting restored
```

Because the move now copies roughly 700 MiB, it takes clearly longer than an eye-blink (about
twenty-five seconds of progress bar), so the cut lands squarely in the middle of a real copy, which
is the whole point. To watch the first recovery run finish without the cut, add `--no-power-cut`.

---

## Honest caveats (say these before you are asked)

- **Shrinking ext4 and NTFS is not in this build.** Their offline resizers are not crash-safe on
  their own, so BootShift wraps them in a guarded run (an undo log the kernel replays after a power
  cut). The undo core is built and crash-tested; the kernel transport is still in progress. Until
  then, this build refuses to shrink ext4 or NTFS, with a plain message, rather than risk it. Btrfs
  shrinks today, which is why the demo uses it.
- **The GUI demo waives authorization.** `demo-gui` starts the helper with the polkit check
  bypassed so you do not type a password mid-demo. That bypass is demo-only. In a real install the
  helper is polkit-gated like any privileged service.
- **Windows booting afterward is the next gate.** Moving NTFS copies and fixes its boot sectors the
  same way, but proving a real Windows 11 install still boots after a move is validation we have not
  finished. We list it as an open risk, not a solved problem.
- **Why not just GParted?** GParted is the engine room: it needs a live USB and it dies with the
  power. BootShift is the layer above that plans the change, applies it in one restart with no
  external media, and treats a power cut as a normal event to resume from, not a disaster.
