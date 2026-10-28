use bootloader::{bootinfo::MemoryRegionType, BootInfo};
use core::arch::x86_64::__cpuid;
use core::fmt::Write;
use crate::disk::IdeDisk;
use crate::fs::FS_START_LBA;

const PAGE_SIZE: u64 = 4096;

struct SerialWriter;
impl Write for SerialWriter { fn write_str(&mut self, string: &str) -> core::fmt::Result { serial_write(string); Ok(()) } }
fn serial_fmt(arguments: core::fmt::Arguments<'_>) { let mut writer = SerialWriter; let _ = writer.write_fmt(arguments); }
macro_rules! serial_println { ($($argument:tt)*) => {{ serial_fmt(format_args!($($argument)*)); serial_write("\n"); }}; }

const PCI_CONFIG_ADDRESS: u16 = 0xcf8;
const PCI_CONFIG_DATA: u16 = 0xcfc;

pub fn report(boot_info: &'static BootInfo, physical_memory_offset: u64) {
    serial_write("\nHardware discovery\n");
    report_cpu();
    report_memory(boot_info);
    report_acpi(physical_memory_offset);
    report_pci();
}

fn report_cpu() {
    // CPUID is mandatory in long mode. Leaf 0 supplies the vendor string;
    // leaf 1 supplies the basic family/model and feature bits.
    let vendor = __cpuid(0);
    let feature = __cpuid(1);
    let mut vendor_bytes = [0u8; 12];
    vendor_bytes[0..4].copy_from_slice(&vendor.ebx.to_le_bytes());
    vendor_bytes[4..8].copy_from_slice(&vendor.edx.to_le_bytes());
    vendor_bytes[8..12].copy_from_slice(&vendor.ecx.to_le_bytes());
    serial_write("cpu vendor: ");
    serial_write(core::str::from_utf8(&vendor_bytes).unwrap_or("unknown"));
    serial_println!("");
    serial_println!("cpu family/model: {}/{}", (feature.eax >> 8) & 0xf, (feature.eax >> 4) & 0xf);
    serial_write("cpu features:");
    if feature.edx & (1 << 25) != 0 { serial_write(" sse"); }
    if feature.edx & (1 << 26) != 0 { serial_write(" sse2"); }
    if feature.ecx & (1 << 0) != 0 { serial_write(" sse3"); }
    if feature.ecx & (1 << 9) != 0 { serial_write(" ssse3"); }
    if feature.ecx & (1 << 19) != 0 { serial_write(" sse4.1"); }
    if feature.ecx & (1 << 20) != 0 { serial_write(" sse4.2"); }
    if feature.ecx & (1 << 28) != 0 { serial_write(" avx"); }
    serial_write("\n");
}

fn report_memory(boot_info: &'static BootInfo) {
    let mut usable_bytes = 0u64;
    let mut regions = 0u32;
    let mut valid = true;
    let mut previous_end = 0u64;
    for region in boot_info.memory_map.iter() {
        let start = region.range.start_addr();
        let end = region.range.end_addr();
        if end < start || (regions != 0 && start < previous_end) {
            valid = false;
        }
        if region.region_type == MemoryRegionType::Usable {
            usable_bytes = usable_bytes.saturating_add(end.saturating_sub(start));
        }
        previous_end = end;
        regions += 1;
    }
    serial_println!("memory map: {} regions, {} KiB usable, {}", regions, usable_bytes / 1024, if valid { "validated" } else { "invalid" });
    serial_println!("memory map page size: {} bytes", PAGE_SIZE);
}

#[derive(Clone, Copy)]
struct Rsdp { address: u64, revision: u8 }

fn report_acpi(physical_memory_offset: u64) {
    match find_rsdp(physical_memory_offset) {
        Some(rsdp) => serial_println!("acpi: validated RSDP revision {} at {:#x}", rsdp.revision, rsdp.address),
        None => serial_write("acpi: no validated RSDP found\n"),
    }
}

fn find_rsdp(physical_memory_offset: u64) -> Option<Rsdp> {
    // BIOS data area 0x40e contains the EBDA segment. The bootloader's BIOS
    // page tables identity-map this low physical memory on the QEMU target.
    let ebda = unsafe { read_u8(physical_memory_offset, 0x40e) as u64 | (read_u8(physical_memory_offset, 0x40f) as u64) << 8 } << 4;
    if let Some(rsdp) = scan_rsdp(physical_memory_offset, ebda, ebda.saturating_add(1024)) { return Some(rsdp); }
    scan_rsdp(physical_memory_offset, 0xe0000, 0x100000)
}

fn scan_rsdp(physical_memory_offset: u64, start: u64, end: u64) -> Option<Rsdp> {
    let start = (start + 15) & !15;
    let mut address = start;
    while address.saturating_add(20) <= end {
        if unsafe { signature_at(physical_memory_offset, address) } && unsafe { checksum(physical_memory_offset, address, 20) } {
            let revision = unsafe { read_u8(physical_memory_offset, address + 15) };
            if revision < 2 { return Some(Rsdp { address, revision }); }
            let length = unsafe { read_u32(physical_memory_offset, address + 20) } as u64;
            if length >= 36 && length <= 4096 && address.saturating_add(length) <= end && unsafe { checksum(physical_memory_offset, address, length as usize) } {
                return Some(Rsdp { address, revision });
            }
        }
        address += 16;
    }
    None
}

unsafe fn signature_at(offset: u64, address: u64) -> bool {
    let signature = *b"RSD PTR ";
    for (index, byte) in signature.iter().enumerate() { if read_u8(offset, address + index as u64) != *byte { return false; } }
    true
}

unsafe fn checksum(offset: u64, address: u64, length: usize) -> bool {
    let mut sum = 0u8;
    for index in 0..length { sum = sum.wrapping_add(read_u8(offset, address + index as u64)); }
    sum == 0
}

unsafe fn read_u8(offset: u64, address: u64) -> u8 { core::ptr::read_volatile((offset + address) as *const u8) }
unsafe fn read_u32(offset: u64, address: u64) -> u32 {
    (read_u8(offset, address) as u32) | (read_u8(offset, address + 1) as u32) << 8 | (read_u8(offset, address + 2) as u32) << 16 | (read_u8(offset, address + 3) as u32) << 24
}

#[derive(Clone, Copy)]
struct PciDevice { bus: u8, slot: u8, function: u8, vendor: u16, device: u16, class: u8, subclass: u8, header_type: u8 }

trait Driver {
    fn status(&self, device: PciDevice) -> &'static str;
}

struct IdePioDriver { usable: bool }

impl Driver for IdePioDriver {
    fn status(&self, device: PciDevice) -> &'static str {
        if device.class == 0x01 && device.subclass == 0x01 {
            if self.usable { "detected, driver initialized, usable: IDE PIO storage" } else { "detected, driver initialized, unusable: IDE PIO sector test failed" }
        } else { "detected, driver: none, usable: no" }
    }
}

fn report_pci() {
    let mut ide_disk = IdeDisk::new();
    let ide_usable = ide_disk.verify_sector_io(FS_START_LBA).is_ok();
    let driver = IdePioDriver { usable: ide_usable };
    serial_println!("ide pio: {}", if ide_usable { "initialized, usable (sector read/write verified)" } else { "initialized, unusable (sector read/write failed)" });
    let mut found = 0u32;
    for bus in 0..=255u16 {
        for slot in 0..32u8 {
            let first = pci_device(bus as u8, slot, 0);
            if first.is_none() { continue; }
            let first = first.unwrap();
            let functions = if first.header_type & 0x80 != 0 { 8 } else { 1 };
            for function in 0..functions { if let Some(device) = pci_device(bus as u8, slot, function) { print_pci(device, &driver); found += 1; } }
        }
    }
    serial_println!("pci: {} device(s) enumerated", found);
}

fn pci_device(bus: u8, slot: u8, function: u8) -> Option<PciDevice> {
    let id = pci_read(bus, slot, function, 0x00);
    let vendor = id as u16;
    if vendor == 0xffff { return None; }
    let class_data = pci_read(bus, slot, function, 0x08);
    let header = pci_read(bus, slot, function, 0x0c);
    Some(PciDevice { bus, slot, function, vendor, device: (id >> 16) as u16, class: (class_data >> 24) as u8, subclass: (class_data >> 16) as u8, header_type: (header >> 16) as u8 })
}

fn print_pci(device: PciDevice, driver: &dyn Driver) {
    serial_println!("pci {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}:{:02x} - {}", device.bus, device.slot, device.function, device.vendor, device.device, device.class, device.subclass, driver.status(device));
}

fn pci_read(bus: u8, slot: u8, function: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32 | (bus as u32) << 16 | (slot as u32) << 11 | (function as u32) << 8 | (offset as u32 & 0xfc);
    unsafe { out_u32(PCI_CONFIG_ADDRESS, address); in_u32(PCI_CONFIG_DATA) }
}

unsafe fn out_u32(port: u16, value: u32) { core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)); }
unsafe fn in_u32(port: u16) -> u32 { let value: u32; core::arch::asm!("in eax, dx", in("dx") port, lateout("eax") value, options(nomem, nostack, preserves_flags)); value }

fn serial_write(string: &str) { for byte in string.bytes() { unsafe { core::arch::asm!("out dx, al", in("dx") 0x3f8u16, in("al") byte, options(nomem, nostack, preserves_flags)); } } }
