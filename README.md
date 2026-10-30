# YaliOS

The first bootable YaliOS kernel: a minimal `no_std` Rust x86_64 kernel that
boots through the `bootloader` crate and runs in QEMU BIOS mode.

## Community

Join the YaliOS community chat:

- **Web:** [chat.w4nn4d13.tech](https://chat.w4nn4d13.tech) — join `#yalios` channel
- **IRC client:** `chat.w4nn4d13.tech` port `6697` (TLS) — channel `#yalios`

## Prerequisites

- Rustup with the nightly toolchain
- `bootimage`: `cargo install bootimage`
- QEMU with `qemu-system-x86_64` on `PATH`

On Windows, install QEMU separately and add its installation directory to
`PATH` before running the commands below.

## Build and run

```text
cargo bootimage
cargo bootimage run
```

The kernel writes `YaliOS 0.1` and `Kernel online` to VGA text memory. The
same banner is sent to COM1, so `cargo bootimage run` displays it in the QEMU
terminal and exits only when QEMU is closed.

The generated disk image is placed under `target\x86_64-...\debug\bootimage-yali_os.bin`.

After boot, the serial console shows a `yali>` prompt. Input can come from
COM1 (the QEMU serial terminal) or a directly attached PS/2 keyboard. The
shell supports `help`, `about`, `version`, `clear`, `echo <text>`, `pwd`, `ls`,
`cd`, `mkdir`, `touch`, `cat`, `write <path> <text>`, `rm`, `mv`, and `cp`.
`ls` accepts `-a`, `-l`, and combined `-la`/`-al`,
plus backspace and Enter editing in a fixed 128-byte command buffer.

## Install and persistence test

Run `build-install-test.cmd` from Windows. It builds the boot image, creates a
fresh 64 MiB image under `target\installed-vhd\yali-os.img`, installs the
bootloader/kernel image at LBA 0, and runs two QEMU boots. The first boot
explicitly formats the newly created image with `format /dev/yali-test` and
writes a file; the second boot mounts the same image and reads it back.

To launch that installed image interactively afterward, run `run-shell.cmd`.
The workflow only creates and overwrites the exact image inside `target` and
does not touch physical disks or Windows boot configuration.

## YaliFS

YaliFS is a small fixed-layout filesystem in the installed test disk. It has a
root directory, nested directories, and text files. Paths may be absolute or
relative and support `.` and `..` where applicable. It is limited to 32 nodes,
24-byte names, and 256-byte file contents. Its metadata begins at LBA 2048,
leaving the boot image at the start of the disk. The format uses a superblock,
two metadata sectors, and one fixed data sector per node; it has no allocator,
permissions, timestamps, journaling, or crash recovery.

## Accounts and permissions

An explicit `setup-users` flow creates `root` (UID 0) and `user` (UID 1000),
requiring passwords of at least eight printable characters. Passwords are
stored as salted SHA-256 hashes, never plaintext. The filesystem persists
owners and Unix-style mode bits; `/home/user` is created as a user-owned 0700
directory. `login`, `whoami`, `id`, and root-only `chmod` are available.

This is not yet a secure Linux-style process boundary. The shell still runs in
kernel mode, and there are no ring-3 processes, per-process address spaces, or
capability isolation. Permission checks are kernel filesystem checks for the
authenticated shell credential only; YaliOS does not claim full user
isolation until process support exists.

## What exists in this first version

- BIOS boot via `bootloader 0.9`
- Rust `no_std` / `no_main` kernel entry point
- VGA text-mode output
- COM1 serial output for headless QEMU runs
- Polling input from COM1 and the PS/2 keyboard (no keyboard IRQs)
- Small fixed-buffer command shell
- Fixed-size persistent YaliFS with basic path and file operations

## Hardware discovery milestone

At boot, YaliOS now reports the CPU vendor/basic features, validates the
bootloader-provided memory map, validates an ACPI RSDP checksum when the BIOS
exposes one, and enumerates PCI configuration space. PCI output distinguishes
detected devices, initialized drivers, and usable drivers. The QEMU IDE PIO
path is marked usable only after a real read/write test on the YaliFS sector;
other devices are only detected. This is discovery, not general hardware support: there
is no PCI interrupt routing, allocator, USB, network, framebuffer, or generic
device-driver framework yet.
- Panic handler that reports a kernel panic and halts
- CPU halt loop to avoid spinning while idle
