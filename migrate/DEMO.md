# BootShift Migrate — demo runbook

Everything is screen-recordable. **No reboot** happens in any migration demo, so no camera is
needed. Each case boots two throwaway VMs, migrates between them, and prints a clear result.

## Setup (once, at the box's monitor)

Open a terminal **on the box's desktop** (not over SSH) so the VM windows and the GUI appear on
screen. Then:

```bash
cd ~/proj/HackmanX
export PATH="$HOME/.cargo/bin:$PATH"
export BOOTSHIFT_VM_DISPLAY=1          # opens a window for each VM, so you can show "two machines"
```

- `BOOTSHIFT_VM_DISPLAY=1` makes QEMU open a GTK window per guest (`DISPLAY=:0` is already set on the
  desktop session). Drop it to run fully headless.
- The prepared guest image already exists, so the first boot is quick.
- **Record the terminal as the hero.** The guest windows are context ("two separate machines"); the
  story — migrating, cutover, receipt, PASSED — is in the terminal.

---

## The five cases

Run each from the setup terminal. Rough timings are with KVM on this box.

### 1. Plain copy (the baseline) — ~2 min
```bash
cargo xtask migrate-rig --data-mib 128
```
Narrate: "A disk on one machine, copied to a blank disk on another over a real TCP link, every
chunk written then read back and verified." **Money shot:** `MIGRATE RIG PASSED … the target is
byte-for-byte identical`.

### 2. Live migration (the headline) — ~2 min
```bash
cargo xtask migrate-rig --live --data-mib 128
```
Narrate: "The source filesystem stays mounted and is being written the whole time. The eBPF tracker
captures every change, we freeze for a moment, and a signed receipt proves the copy is exact."
**Money shot:** `migration committed. Data downtime ~1000 ms. Receipt signed … roots match`.

### 3. Reshape (migrate and resize in one step) — ~2 min
```bash
cargo xtask migrate-rig --reshape --data-mib 64
```
Narrate: "The source is a 64 MiB disk migrated onto a disk 3× its size, then grown to fill it — one
step, no downtime to resize later." **Money shot:** `finalize grew the partition and ext4
filesystem … GROWN_MIB=190 … FILES-OK`.

### 4. Mirror mode (synchronous, seconds of downtime) — **~4–5 min** (installs nvme modules first)
```bash
cargo xtask migrate-rig --mirror --data-mib 128
```
Narrate: "For the busiest machines, the target exports a disk over nvme-tcp and the kernel mirrors
every write to both sides with LVM raid1. At cutover the target promotes its copy." **Money shot:**
`legs in sync (100%) … promoted … every file intact`. Note: the first part installs the nvme-tcp
modules into the guest, which is the slow bit — cut it or talk over it. (Ask me to pre-bake the
modules into the image if you want this fast.)

### 5. Rehearsal (test the replica safely) — ~2 min
```bash
cargo xtask migrate-rig --rehearse --data-mib 128
```
Narrate: "Before you trust the migration, boot and test a writable snapshot of the replica — your
test writes go to a copy-on-write device and never touch the real target." **Money shot:**
`SNAP-FILES-OK … REPLICA-CLEAN … the rehearsal's own write never reached the replica`.

---

## The dashboard GUI (a visual for the live story)

On the box's desktop:
```bash
cargo run -p bootshift-migrate-gui
```
A window opens and animates a converging migration: the **churn bars** shrink pass by pass, the
**forecast** turns green to "Ready to cut over — predicted data downtime …", you click **Cut over
now**, and the **signed receipt** panel appears (target equals source, signature valid, downtime).
Screen-record this window for the live/cutover beat.

---

## Verify the receipt on camera (nice closer)

After a live run writes a receipt, show that anyone can check it later:
```bash
# (the live rig's receiver can save one with: receive … --receipt /tmp/receipt.json)
bootshift-migrate verify-receipt /tmp/receipt.json --target <disk>
```
**Money shot:** `the receipt verifies: the target equals the source, signed by receiver key …`.

---

---

## BootShift partition mover (the "reboot" one) — fully recordable

This is the original BootShift: drag a partition, apply, and it moves in one restart. The restart is
the **guest VM rebooting into the recovery console** — it happens inside the QEMU window on your
screen, so you record that window. **The box itself never reboots; no camera needed.**

Run these on the box's desktop (a QEMU window opens on screen):

### A. GUI-driven move (the visual one) — ~5 min
```bash
cargo xtask demo-gui
```
It boots a guest, starts the real `bootshiftd` helper in it, and prints the command to launch the
desktop app against the guest's real disk. Then, on screen:
1. The BootShift window opens — **drag a partition**, run the checks, click apply.
2. When it says to restart, reboot the guest (command is printed). **The QEMU window shows the
   BootShift recovery console moving the partition, live** — this is the "reboot bit", on screen.
3. After it boots back, reopen the app to show the new layout.

### B. Crash-safety proof (the showstopper) — ~6 min, automated
```bash
cargo xtask boot-proof
```
Fully automated end to end, narrated in the terminal while the QEMU window shows the recovery
console: it applies a partition move, reboots into recovery, **cuts the power mid-move (~9%)**,
powers back on, resumes from the journal, and verifies every file is byte-identical and the boot
order restored. **Money shot:** `BOOT PROOF PASSED … survived a power cut mid-move and resumed from
the journal … every file byte-identical`.

Record the QEMU window for the recovery console and the terminal for the narration. For the power
cut, `boot-proof` kills the VM itself — on camera it looks like pulling the plug, and the resume is
the magic.

---

## Tips

- Run order for a single take: copy → live → reshape → rehearse → mirror (mirror last, since it is
  the slow one).
- Each rig ends by powering off both VMs, so windows close cleanly between cases.
- If a run is interrupted, leftover VMs: `pkill -f qemu-system`.
- Fully headless (for an asciinema terminal recording) is the same commands without
  `BOOTSHIFT_VM_DISPLAY=1`.
