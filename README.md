# YaliOS

The first bootable YaliOS kernel: a minimal `no_std` Rust x86_64 kernel that
boots through the `bootloader` crate and runs in QEMU BIOS mode.

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

## What exists in this first version

- BIOS boot via `bootloader 0.9`
- Rust `no_std` / `no_main` kernel entry point
- VGA text-mode output
- COM1 serial output for headless QEMU runs
- Panic handler that reports a kernel panic and halts
- CPU halt loop to avoid spinning while idle
