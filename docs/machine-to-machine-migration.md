# Idea: Machine-to-machine migration with BootShift

A handoff note for a new working session. It describes the idea, the market it enters and why BootShift's pipeline gives it an edge. It is not an implementation plan.

> **Canonical design is `migrate/plan.md` + `architecture.md` + `implementation.md`; this note is superseded by them.** Competitive claims below were re-verified against vendor docs in October 2026 and corrected: the three differentiators that actually hold are **reshape**, **signed proof**, and a **read-only (never-written) source**. "Nothing installed on the old machine" was an overclaim — v1 runs a root binary to *read* the source; it is agentless in the sense of *no kernel module and no writes to the source*, not *no software run*.

## The problem

An organisation has a machine that will get no more updates: the OS version or the hardware has reached end of life. Everything on it, the operating system, apps and data, has to move to a new machine. The old machine is still in use and keeps writing data, so it can't simply be switched off for hours while a copy runs.

**Goal:** move the whole machine to new hardware with as little downtime as possible, without losing or corrupting a single byte, and without installing anything on the old machine.

Zero downtime isn't the claim. Some downtime always happens at the switchover. The goal is to make it short and safe, and to prove it was safe.

## How the market does it today

**AWS Application Migration Service (MGN)**
- Installs an agent with a kernel driver on the source machine, which must be built against that exact kernel.
- Copies the whole disk to AWS once, then keeps sending changed blocks.
- Lets you test-launch a copy in AWS without disturbing the source.
- Cutover does a final sync and starts the server in AWS, with a few minutes of downtime.
- Only goes to AWS.

**Nutanix Move**
- Works on virtual machines only. Uses hypervisor snapshots instead of an agent.
- Copies a first snapshot, then sends changes every 10 minutes or so.
- Cutover shuts the VM down, does a final sync, fixes drivers and starts it on Nutanix.
- Only goes to Nutanix. Under heavy writes it can fall back to copying everything again.

**VMware storage migration (research paper, USENIX ATC 2011)**
- Tried three methods over the years:
  - **Snapshots:** slow, and the switchover isn't atomic.
  - **Repeated passes over changed blocks:** 7–13 s of downtime, and it failed outright when the write load doubled.
  - **Mirroring every new write to both sides while copying once:** about 0.2 s of downtime, and it always finishes.
- The lesson: mirroring writes beats chasing changed blocks.

**Others:** Clonezilla-style offline cloning (hours of downtime), DRBD (needs a kernel module and a remount), rsync (file-level, needs repeated full scans).

**Common pattern:** copy everything once → keep sending changes → optionally test → short final sync and switch.

## Our idea: BootShift Migrate

Offer the same four steps, for **any machine to any machine**: bare metal, any VM, any cloud. Build it on the engine BootShift already has.

1. **Copy while the old machine runs.** Copy only the space that's in use, not the whole disk.
2. **Keep up with new writes.**
   - **If the machine uses LVM** (common on Ubuntu installs), mirror every write to the new machine, the method VMware found best. It always finishes, and the switchover is close to instant.
   - **Otherwise,** a lightweight tracker records which blocks changed, and they're re-sent in short passes.
3. **Rehearse.** Start a throwaway copy on the new machine to check that everything works, while copying carries on. You could even rehearse an OS upgrade there first.
4. **Switch over.**
   - Pause writes for a few seconds and send the last changes.
   - Start the new machine quickly, without a full firmware restart.
   - Move the network address to the new machine.
   - The old machine is left untouched as a fallback.

## Why it can beat them (the USP)

1. **No kernel module, and the source is read-only.** The agent is a single static binary that *reads* the source: no kernel driver, no packages, and it never writes the source's data. (Competitors install an in-guest agent, or snapshot the source datastore each cycle.) This suits machines that are frozen or out of support, which is exactly when people migrate. ("Nothing installed" was an overclaim; the honest version is *no kernel module, source never written*.)
2. **Survives a power cut or network drop.** BootShift's journal already resumes from the last finished piece, and its crash tests cover every write. Competitors resync or start over.
3. **Proof, not a promise.** Every piece is read back and checked, and a chain of hashes produces a signed receipt showing that the new machine matches the old one at switchover. **No migration product ships a receipt anyone can re-verify:** Azure checksums internally and keeps nothing, and everywhere else integrity validation is a manual step. Useful for audits and compliance.
4. **Rehearsal anywhere.** AWS only lets you test inside AWS, and Move has no rehearsal at all. We rehearse on the target machine itself, and can include the OS upgrade.
5. **The new disk doesn't have to match, and we reshape it automatically.** BootShift's planner resizes and rearranges partitions and grows filesystems as part of the migration, so a different size or layout is fine. Competitors clone at the source size, or (MGN) hand you a bigger raw volume you must grow by hand; none reshape the layout. Disk and partition IDs stay the same, so the system still boots.
6. **Seconds of downtime, and an easy undo.** The old machine is never modified after the switchover pause, so going back is just turning it on again.
7. **No lock-in.** Open source, and any target works.

**Pitch:** *Migrate any Linux machine onto any new disk — automatically reshaped and grown to fit — with a signed receipt proving it is byte-for-byte identical, and the source never written, so rollback is one power button. No kernel module; survives power cuts.*

## What BootShift already brings

| Need | Already in BootShift |
|---|---|
| Safe, resumable, verified copying | The copier and journal |
| Copying only used space | The filesystem adapters |
| Refusing unsafe situations before touching anything | Preflight checks |
| A minimal recovery system to run from | The recovery boot image |
| Fitting partitions to a different disk | The layout planner and resize steps |
| Keeping disk and partition identity | Already a safety rule |

**New pieces needed:**
- sending data over the network;
- the change tracker for non-LVM machines;
- network-attaching the new disk for LVM mirroring;
- adapting the new machine to different hardware (drivers, boot entry);
- the switchover step itself.

## Risks to check before building

- Can the change tracker run on a locked-down, Secure Boot machine?
- Is the tracker complete? Every kind of write must be caught; if in doubt, fall back to comparing the whole disk.
- The LVM mirror makes every write wait for the network, so it needs a fast local network. It also changes the old machine's volume layout, which the customer must agree to.
- Can the fast start of the new machine work under Secure Boot?
- Is the rehearsal copy truly isolated from the real replica?

## Open questions for the product

- Who is the first customer: home users moving to a new laptop, small businesses, or data centres retiring servers?
- Is an OS upgrade (for example Ubuntu 22.04 → 24.04) part of the offer, or only moving the machine as it is?
- Do we support Windows as a source, or Linux only at first?
- Should the "verified switchover" (longer pause, full comparison) or the "fast switchover" (seconds) be the default? BootShift's one rule, never lose data, argues for verified by default with fast as an opt-in.

## References

- AWS MGN: [overview](https://aws.amazon.com/blogs/aws/how-to-use-the-new-aws-application-migration-service-for-lift-and-shift-migrations), [launch and cutover](https://docs.aws.amazon.com/mgn/latest/ug/launch-cutover.md), [agent FAQ](https://docs.aws.amazon.com/drs/latest/userguide/Agent-Related-FAQ.html)
- Nutanix Move: [solution brief](https://www.nutanix.com/content/dam/nutanix/en/resources/solution-briefs/sb-move-application-migration.pdf), [DirectPath](https://www.nutanix.com/blog/flexible-vm-migrations-to-the-nutanix-cloud-platform-with-move-directpath)
- [The Design and Evolution of Live Storage Migration in VMware ESX (USENIX ATC '11)](https://usenix.org/conference/usenixatc11/design-and-evolution-live-storage-migration-vmware-esx)
- [Live Migration of Virtual Machines (NSDI '05)](https://static.usenix.org/event/nsdi05/tech/full_papers/clark/clark_html)
- [XReplicator: change tracking without a kernel module](https://news.ycombinator.com/item?id=47527328)
- Linux building blocks: [dm-clone](https://docs.kernel.org/admin-guide/device-mapper/dm-clone.html), [dm-era](https://docs.kernel.org/6.13/admin-guide/device-mapper/era.html), [bdsync](https://github.com/Slimmer-AI/bdsync)
