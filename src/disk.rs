pub const SECTOR_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    Timeout,
    DeviceError,
}

pub trait BlockDevice {
    fn read_sector(&mut self, lba: u32, buffer: &mut [u8; SECTOR_SIZE]) -> Result<(), DiskError>;
    fn write_sector(&mut self, lba: u32, buffer: &[u8; SECTOR_SIZE]) -> Result<(), DiskError>;
}

pub struct IdeDisk;

const DATA_BASE: u16 = 0x1f0;
const DATA_SELECT: u16 = 0x1f6;

impl IdeDisk {
    pub const fn new() -> Self {
        Self
    }

    fn wait_ready(&self) -> Result<u8, DiskError> {
        for _ in 0..1_000_000 {
            let status = unsafe { inb(DATA_BASE + 7) };
            if status & 0x01 != 0 {
                return Err(DiskError::DeviceError);
            }
            if status & 0x80 == 0 {
                return Ok(status);
            }
        }
        Err(DiskError::Timeout)
    }

    fn select(&self, lba: u32) {
        // The installed YaliOS image is QEMU's primary IDE master.
        unsafe { outb(DATA_SELECT, 0xa0 | ((lba >> 24) as u8 & 0x0f)) };
        unsafe { outb(DATA_BASE + 2, 1) };
        unsafe { outb(DATA_BASE + 3, lba as u8) };
        unsafe { outb(DATA_BASE + 4, (lba >> 8) as u8) };
        unsafe { outb(DATA_BASE + 5, (lba >> 16) as u8) };
    }

    pub fn verify_sector_io(&mut self, lba: u32) -> Result<(), DiskError> {
        let mut sector = [0u8; SECTOR_SIZE];
        self.read_sector(lba, &mut sector)?;
        self.write_sector(lba, &sector)
    }
}

impl BlockDevice for IdeDisk {
    fn read_sector(&mut self, lba: u32, buffer: &mut [u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        self.select(lba);
        unsafe { outb(DATA_BASE + 7, 0x20) };
        let status = self.wait_ready()?;
        if status & 0x08 == 0 {
            return Err(DiskError::DeviceError);
        }
        for word in buffer.chunks_exact_mut(2) {
            let value = unsafe { inw(DATA_BASE) };
            word[0] = value as u8;
            word[1] = (value >> 8) as u8;
        }
        Ok(())
    }

    fn write_sector(&mut self, lba: u32, buffer: &[u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        self.select(lba);
        unsafe { outb(DATA_BASE + 7, 0x30) };
        let status = self.wait_ready()?;
        if status & 0x08 == 0 {
            return Err(DiskError::DeviceError);
        }
        for word in buffer.chunks_exact(2) {
            unsafe { outw(DATA_BASE, u16::from_le_bytes([word[0], word[1]])) };
        }
        unsafe { outb(DATA_BASE + 7, 0xe7) };
        self.wait_ready().map(|_| ())
    }
}

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value,
        options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", in("dx") port, lateout("al") value,
        options(nomem, nostack, preserves_flags));
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", in("dx") port, lateout("ax") value,
        options(nomem, nostack, preserves_flags));
    value
}

unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value,
        options(nomem, nostack, preserves_flags));
}
