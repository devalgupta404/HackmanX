# Security Policy

BootShift runs with root privileges and writes directly to disks and the partition table. A flaw can corrupt or destroy data, or be used to tamper with a system at boot. We take reports seriously.

## Supported versions

BootShift is under active development and has not yet been declared stable. Security and data-safety fixes land on `main`. Until a tagged release exists, only `main` is supported.

> **Note:** BootShift is not yet tested on real hardware. Do not run it on a disk you care about.

## Reporting a vulnerability

**Please do not open a public issue for security or data-safety vulnerabilities.**

Report privately through GitHub's **[Private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability)** (the **Security** tab → **Report a vulnerability**). If that is unavailable, open a minimal public issue that says only "security report, please provide a private contact" and we will follow up.

Please include, where you can:

- a description of the issue and why it is a risk;
- the affected component (crate, the recovery path, the boot redirect, the D-Bus helper, etc.);
- a reproduction: the disk layout, filesystem, and steps, ideally against `MemDisk`/`CrashDisk` or a loop device in the test VM, never a real disk;
- the impact (data corruption, privilege escalation, boot tampering, denial of service).

We aim to acknowledge a report within a few days and to agree on a disclosure timeline with you.

## What counts as a security or safety issue

Because this tool touches data and boot, the following are in scope alongside classic vulnerabilities:

- **Data loss or corruption**: any path that can leave a partition or filesystem damaged, including crashes mid-move that the journal cannot resume cleanly.
- **A violated safety invariant**: for example writing before preflight passes, changing the partition table before data is verified in place, or marking a chunk done before `fdatasync` returns.
- **Privilege or boot issues**: escaping the polkit check on the D-Bus helper, or a boot redirect that could run unverified code at startup.

## Our commitments

- We will not weaken or bypass a safety check or crash test to ship a fix faster.
- A fix for a data-safety bug starts with a failing test that reproduces it, so the same bug cannot return.
- We will credit reporters who want to be credited once a fix is released.
