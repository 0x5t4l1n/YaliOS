#![no_std]
#![no_main]

use bootloader::{entry_point, BootInfo};
use core::fmt::Write;
use spin::Mutex;

entry_point!(kernel_main);

/// A minimal VGA text-mode writer. QEMU displays this at address `0xb8000`.
struct VgaWriter {
    column: usize,
    color: u8,
}

impl VgaWriter {
    const fn new() -> Self {
        Self {
            column: 0,
            color: 0x0f, // bright white on black
        }
    }

    fn write_byte(&mut self, byte: u8) {
        if byte == b'\n' || self.column >= 80 {
            self.new_line();
            if byte == b'\n' {
                return;
            }
        }

        let cell = 0xb8000 as *mut u8;
        let offset = self.column * 2;
        // SAFETY: VGA text memory is mapped by the BIOS at 0xb8000 in BIOS boots.
        unsafe {
            cell.add(offset).write_volatile(byte);
            cell.add(offset + 1).write_volatile(self.color);
        }
        self.column += 1;
    }

    fn new_line(&mut self) {
        self.column = 0;
        // Keep the boot banner short and deterministic for the first version.
    }
}

impl Write for VgaWriter {
    fn write_str(&mut self, string: &str) -> core::fmt::Result {
        for byte in string.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

static VGA_WRITER: Mutex<VgaWriter> = Mutex::new(VgaWriter::new());

#[inline]
fn halt() {
    // SAFETY: `hlt` is valid in the kernel's ring-0 execution context.
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn serial_out(port: u16, value: u8) {
    // SAFETY: callers use the documented 16550 COM1 ports.
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

/// Initialize and write to the first serial port (COM1), which is connected to
/// QEMU's stdio.
fn serial_init() {
    // 38400 baud, 8 data bits, no parity, one stop bit (8N1).
    unsafe {
        serial_out(0x3f9, 0x00); // Disable interrupts.
        serial_out(0x3fb, 0x80); // Enable divisor latch access.
        serial_out(0x3f8, 0x03); // Divisor low byte: 38400 baud.
        serial_out(0x3f9, 0x00); // Divisor high byte.
        serial_out(0x3fb, 0x03); // 8N1, disable divisor latch.
        serial_out(0x3fa, 0xc7); // Enable FIFO and clear it.
        serial_out(0x3fc, 0x0b); // IRQs enabled, RTS/DSR set.
    }
}

fn serial_write(string: &str) {
    for byte in string.bytes() {
        unsafe {
            serial_out(0x3f8, byte);
        }
    }
}

fn banner() {
    let message = "YaliOS 0.1\nKernel online\n";
    serial_init();
    let _ = VGA_WRITER.lock().write_str(message);
    serial_write(message);
}

fn kernel_main(_boot_info: &'static BootInfo) -> ! {
    banner();

    loop {
        halt();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut writer = VGA_WRITER.lock();
    let _ = writer.write_str("\nKERNEL PANIC: ");
    let _ = writer.write_fmt(format_args!("{}", info));
    drop(writer);

    serial_write("\nKERNEL PANIC\n");
    loop {
        halt();
    }
}
