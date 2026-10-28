#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod disk;
mod fs;
mod hardware;
mod auth;

use bootloader::{entry_point, BootInfo};
use core::fmt::{self, Write};
use disk::IdeDisk;
use auth::ROOT_UID;
use fs::{FileSystem, FsError};
use spin::{Mutex, Once};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

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

struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, string: &str) -> fmt::Result {
        for byte in string.bytes() {
            unsafe {
                serial_out(0x3f8, byte);
            }
        }
        Ok(())
    }
}

static IDT: Once<InterruptDescriptorTable> = Once::new();
const COMMAND_BUFFER_SIZE: usize = 128;

fn init_idt() {
    IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.double_fault.set_handler_fn(double_fault_handler);
        idt
    })
    .load();
}

#[inline]
fn halt() {
    // SAFETY: `hlt` is valid in the kernel's ring-0 execution context.
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

fn enable_sse() {
    // The bootloader leaves OSFXSR disabled. Rust's interrupt-handler prologue
    // may save XMM registers, so enable the architectural SSE support first.
    unsafe {
        core::arch::asm!(
            "mov rax, cr0",
            "and rax, 0xfffffffffffffffB",
            "or rax, 0x2",
            "mov cr0, rax",
            "mov rax, cr4",
            "or rax, 0x600",
            "mov cr4, rax",
            options(nomem, nostack, preserves_flags)
        );
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
    let _ = SerialWriter.write_str(string);
}

unsafe fn serial_in(port: u16) -> u8 {
    let value: u8;
    // SAFETY: callers use the documented 16550 COM1 ports.
    core::arch::asm!(
        "in al, dx",
        in("dx") port,
        lateout("al") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

fn serial_read() -> Option<u8> {
    // COM1 line status register: bit 0 means a byte is ready.
    if unsafe { serial_in(0x3fd) } & 0x01 != 0 {
        Some(unsafe { serial_in(0x3f8) })
    } else {
        None
    }
}

unsafe fn port_in(port: u16) -> u8 {
    serial_in(port)
}

fn keyboard_read() -> Option<u8> {
    // Poll the PS/2 controller status port. No keyboard IRQ is required.
    if unsafe { port_in(0x64) } & 0x01 == 0 {
        return None;
    }

    let scancode = unsafe { port_in(0x60) };
    if scancode & 0x80 != 0 {
        return None;
    }

    Some(match scancode {
        0x01 => 0x1b, // Escape
        0x0e => 8,    // Backspace
        0x1c => b'\n',
        0x39 => b' ',
        0x02..=0x0d => b"1234567890-="[(scancode - 0x02) as usize],
        0x10..=0x19 => b"qwertyuiop"[(scancode - 0x10) as usize],
        0x1e..=0x26 => b"asdfghjkl"[(scancode - 0x1e) as usize],
        0x2c..=0x32 => b"zxcvbnm"[(scancode - 0x2c) as usize],
        0x0f => b'\t',
        0x1a => b'[',
        0x1b => b']',
        0x27 => b';',
        0x28 => b'\'',
        0x2b => b'\\',
        0x33 => b',',
        0x34 => b'.',
        0x35 => b'/',
        _ => return None,
    })
}

fn serial_fmt(arguments: fmt::Arguments<'_>) {
    let _ = SerialWriter.write_fmt(arguments);
}

macro_rules! serial_println {
    ($($argument:tt)*) => {
        serial_fmt(format_args!($($argument)*));
        serial_write("\n");
    };
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("[EXCEPTION] breakpoint");
    serial_println!(
        "  instruction_pointer={:#x}",
        stack_frame.instruction_pointer.as_u64()
    );
    halt_forever();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    serial_println!("[EXCEPTION] double fault");
    serial_println!("  error_code={:#x}", error_code);
    serial_println!(
        "  instruction_pointer={:#x}",
        stack_frame.instruction_pointer.as_u64()
    );
    halt_forever();
}

fn halt_forever() -> ! {
    loop {
        halt();
    }
}

fn print_prompt(fs: &FileSystem<IdeDisk>) {
    let mut output = SerialWriter;
    serial_write("yali:");
    fs.write_pwd(&mut output);
    serial_write("> ");
}

fn write_decimal(mut number: usize) {
    let mut digits = [0u8; 20]; let mut length = 0;
    if number == 0 { serial_write("0"); return; }
    while number > 0 { digits[length] = b'0' + (number % 10) as u8; length += 1; number /= 10; }
    for index in (0..length).rev() { unsafe { serial_out(0x3f8, digits[index]); } }
}

fn parse_octal(input: &str) -> Option<u16> {
    if input.is_empty() || input.len() > 3 { return None; }
    let mut value = 0u16;
    for byte in input.bytes() { if !(b'0'..=b'7').contains(&byte) { return None; } value = value * 8 + (byte - b'0') as u16; }
    Some(value)
}

fn password_nonce() -> u64 {
    let low: u32; let high: u32;
    unsafe { core::arch::asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags)); }
    ((high as u64) << 32) | low as u64
}

fn print_fs_error(error: FsError) {
    serial_write("error: ");
    serial_write(match error {
        FsError::InvalidPath => "invalid path",
        FsError::Missing => "missing path",
        FsError::NotDirectory => "not a directory",
        FsError::IsDirectory => "is a directory",
        FsError::AlreadyExists => "already exists",
        FsError::DirectoryNotEmpty => "directory not empty",
        FsError::NoSpace => "filesystem full",
        FsError::NameTooLong => "name too long",
        FsError::FileTooLarge => "file too large",
        FsError::NotFormatted => "installed disk is not formatted; use format /dev/yali-test",
        FsError::Disk => "disk I/O error",
        FsError::PermissionDenied => "permission denied",
        FsError::Authentication => "authentication failed",
    });
    serial_write("\n");
}

fn split_first_word(input: &str) -> (&str, &str) {
    let input = input.trim_start_matches(' ');
    match input.find(' ') {
        Some(index) => (&input[..index], input[index..].trim_start_matches(' ')),
        None => (input, ""),
    }
}

fn parse_ls(input: &str) -> (bool, bool, Option<&str>, bool) {
    let mut show_hidden = false;
    let mut long = false;
    let mut path = None;
    let mut invalid = false;
    for word in input.split(' ').filter(|word| !word.is_empty()) {
        if path.is_none() && word.as_bytes().first() == Some(&b'-') {
            if word.len() == 1 { invalid = true; continue; }
            for flag in word.as_bytes()[1..].iter() {
                match flag { b'a' => show_hidden = true, b'l' => long = true, _ => invalid = true }
            }
        } else if path.is_none() {
            path = Some(word);
        } else {
            invalid = true;
        }
    }
    (show_hidden, long, path, invalid)
}

enum ShellAction { None, BeginLogin, BeginSetup }

fn execute_command(command: &[u8], fs: &mut FileSystem<IdeDisk>, uid: u32) -> ShellAction {
    let command = core::str::from_utf8(command).unwrap_or("");
    let (name, argument) = split_first_word(command);
    let mut output = SerialWriter;
    match name {
        "help" => serial_write("commands: help about version clear echo <text> pwd ls [-a] [-l] [path] cd mkdir touch cat write <path> <text> rm mv <src> <dst> cp <src> <dst> chmod <mode> <path> login logout whoami id setup-users\n"),
        "about" => serial_write("YaliOS is a small Rust x86_64 kernel.\n"),
        "version" => serial_write("YaliOS 0.1\n"),
        "id" => { serial_write("uid="); write_decimal(uid as usize); serial_write(if uid == ROOT_UID { " name=root\n" } else { " name=user\n" }); },
        "whoami" => serial_write(if uid == ROOT_UID { "root\n" } else { "user\n" }),
        "login" => return ShellAction::BeginLogin,
        "logout" => return ShellAction::BeginLogin,
        "setup-users" if !fs.users_configured() && uid == ROOT_UID => return ShellAction::BeginSetup,
        "setup-users" => serial_write("error: accounts are already configured\n"),
        "clear" => serial_write("\x1b[2J\x1b[H"),
        "echo" => {
            serial_write(argument);
            serial_write("\n");
        }
        "pwd" if argument.is_empty() => {
            let _ = fs.print_pwd(&mut output);
        }
        "ls" => {
            let (show_hidden, long, path, invalid) = parse_ls(argument);
            if invalid { serial_write("error: usage: ls [-a] [-l] [path]\n"); }
            else if let Err(error) = fs.print_ls_as(uid, path, show_hidden, long, &mut output) { print_fs_error(error); }
        }
        "cd" | "mkdir" | "touch" | "cat" | "rm" if argument.is_empty() => {
            serial_write("error: missing operand\n");
        }
        "cd" => if let Err(error) = fs.change_dir_as(uid, argument) { print_fs_error(error); },
        "mkdir" => if let Err(error) = fs.make_dir_as(uid, argument) { print_fs_error(error); },
        "touch" => if let Err(error) = fs.touch_as(uid, argument) { print_fs_error(error); },
        "cat" => if let Err(error) = fs.print_file_as(uid, argument, &mut output) { print_fs_error(error); },
        "rm" => if let Err(error) = fs.remove_as(uid, argument) { print_fs_error(error); },
        "write" => {
            let (path, text) = split_first_word(argument);
            if path.is_empty() || text.is_empty() {
                serial_write("error: usage: write <path> <text>\n");
            } else if let Err(error) = fs.write_file_as(uid, path, text) {
                print_fs_error(error);
            }
        }
        "mv" | "cp" => {
            let (source, target) = split_first_word(argument);
            if source.is_empty() || target.is_empty() {
                serial_write("error: usage: mv|cp <source> <target>\n");
            } else {
                let result = if name == "mv" { fs.move_path_as(uid, source, target) } else { fs.copy_file_as(uid, source, target) };
                if let Err(error) = result { print_fs_error(error); }
            }
        }
        "chmod" => {
            let (mode, path) = split_first_word(argument);
            if let Some(mode) = parse_octal(mode) { if let Err(error) = fs.chmod_as(uid, path, mode) { print_fs_error(error); } }
            else { serial_write("error: usage: chmod <octal-mode> <path>\n"); }
        }
        "format" if argument == "/dev/yali-test" => {
            serial_write("formatting the installed YaliOS test disk\n");
            if uid != ROOT_UID { serial_write("error: permission denied\n"); } else if let Err(error) = fs.format() { print_fs_error(error); }
        }
        "format" => serial_write("error: refusing to format; use exactly: format /dev/yali-test\n"),
        "" => {}
        _ => {
            serial_write("unknown command: ");
            serial_write(command);
            serial_write("\n");
        }
    }
    ShellAction::None
}

enum InputMode { Command, LoginUser, LoginPassword, SetupRootPassword, SetupUserPassword }

fn shell() -> ! {
    let mut buffer = [0u8; COMMAND_BUFFER_SIZE];
    let mut length = 0;
    let mut secret = [0u8; 64];
    let mut secret_length = 0usize;
    let mut root_secret = [0u8; 64];
    let mut root_secret_length = 0usize;
    let mut login_name = [0u8; 8];
    let mut login_name_length = 0usize;
    let mut suppress_lf = false;
    let mut mode;
    let mut uid = ROOT_UID;
    let mut fs = FileSystem::new(IdeDisk::new());
    match fs.mount() {
        Ok(()) => serial_write("YaliFS mounted from the installed disk\n"),
        Err(FsError::NotFormatted) => serial_write("YaliFS: installed disk is not formatted; use format /dev/yali-test\n"),
        Err(error) => print_fs_error(error),
    }

    serial_write("\nYaliOS shell\n");
    if fs.users_configured() { mode = InputMode::LoginUser; serial_write("login: "); }
    else { mode = InputMode::Command; serial_write("accounts are not configured; run setup-users\n"); print_prompt(&fs); }

    loop {
        let input = serial_read().or_else(keyboard_read);
        if let Some(byte) = input {
            if byte == b'\n' && suppress_lf { suppress_lf = false; continue; }
            suppress_lf = byte == b'\r';
            match mode {
                InputMode::Command => match byte {
                    b'\r' | b'\n' => { serial_write("\n"); let action = execute_command(&buffer[..length], &mut fs, uid); length = 0; match action { ShellAction::None => print_prompt(&fs), ShellAction::BeginLogin => { uid = ROOT_UID; mode = InputMode::LoginUser; serial_write("login: "); }, ShellAction::BeginSetup => { secret_length = 0; mode = InputMode::SetupRootPassword; serial_write("root password (8+ chars): "); } } }
                    8 | 127 if length > 0 => { length -= 1; serial_write("\x08 \x08"); }
                    0x20..=0x7e if length < COMMAND_BUFFER_SIZE - 1 => { buffer[length] = byte; length += 1; unsafe { serial_out(0x3f8, byte) }; }
                    _ => {}
                },
                InputMode::LoginUser => match byte {
                    b'\r' | b'\n' => { serial_write("\n"); if length == 0 || length > 8 { serial_write("login failed\nlogin: "); } else { login_name[..length].copy_from_slice(&buffer[..length]); login_name_length = length; secret_length = 0; mode = InputMode::LoginPassword; serial_write("password: "); } length = 0; }
                    8 | 127 if length > 0 => { length -= 1; serial_write("\x08 \x08"); }
                    0x20..=0x7e if length < 8 => { buffer[length] = byte; length += 1; unsafe { serial_out(0x3f8, byte) }; }
                    _ => {}
                },
                InputMode::LoginPassword => match byte {
                    b'\r' | b'\n' => { serial_write("\n"); let name = core::str::from_utf8(&login_name[..login_name_length]).unwrap_or(""); let password = core::str::from_utf8(&secret[..secret_length]).unwrap_or(""); if let Some(credentials) = fs.authenticate(name, password) { uid = credentials.uid; let _ = fs.change_dir_as(uid, "/"); mode = InputMode::Command; serial_write("login successful\n"); print_prompt(&fs); } else { serial_write("login failed\nlogin: "); mode = InputMode::LoginUser; } length = 0; secret_length = 0; }
                    8 | 127 if secret_length > 0 => { secret_length -= 1; }
                    0x20..=0x7e if secret_length < secret.len() => { secret[secret_length] = byte; secret_length += 1; }
                    _ => {}
                },
                InputMode::SetupRootPassword | InputMode::SetupUserPassword => match byte {
                    b'\r' | b'\n' => { serial_write("\n"); if secret_length < 8 { serial_write("password must be at least 8 characters\n"); } else if matches!(mode, InputMode::SetupRootPassword) { root_secret[..secret_length].copy_from_slice(&secret[..secret_length]); root_secret_length = secret_length; secret_length = 0; mode = InputMode::SetupUserPassword; serial_write("user password (8+ chars): "); } else { let root = core::str::from_utf8(&root_secret[..root_secret_length]).unwrap_or(""); let user = core::str::from_utf8(&secret[..secret_length]).unwrap_or(""); match fs.setup_users(root, user, password_nonce()) { Ok(()) => { uid = ROOT_UID; mode = InputMode::Command; serial_write("accounts created; logged in as root\n"); print_prompt(&fs); }, Err(error) => { print_fs_error(error); mode = InputMode::Command; print_prompt(&fs); } } secret_length = 0; } }
                    8 | 127 if secret_length > 0 => { secret_length -= 1; }
                    0x20..=0x7e if secret_length < secret.len() => { secret[secret_length] = byte; secret_length += 1; }
                    _ => {}
                },
            }
        } else {
            core::hint::spin_loop();
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
    enable_sse();
    banner();
    init_idt();
    hardware::report(_boot_info, _boot_info.physical_memory_offset);
    shell();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut writer = VGA_WRITER.lock();
    let _ = writer.write_str("\nKERNEL PANIC: ");
    let _ = writer.write_fmt(format_args!("{}", info));
    drop(writer);

    serial_write("\nKERNEL PANIC\n");
    halt_forever();
}
