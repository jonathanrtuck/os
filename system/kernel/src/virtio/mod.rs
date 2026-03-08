//! Virtio MMIO transport layer (QEMU `virt`).
//!
//! QEMU's `virt` machine exposes 32 virtio-mmio slots starting at PA
//! 0x0a00_0000, each 0x200 bytes apart. SPI interrupts start at GIC IRQ 48
//! (SPI 16). This module handles device probing, feature negotiation, and
//! the MMIO register interface. Device drivers (console, blk) build on top.
//!
//! Supports both legacy (v1) and modern (v2) transports, but queue setup
//! uses v2 registers. Pass `-global virtio-mmio.force-legacy=false` to QEMU
//! to enable modern transport (QEMU defaults to legacy).

pub mod blk;
pub mod console;
pub mod virtqueue;

use super::memory::KERNEL_VA_OFFSET;
use super::mmio;
use super::uart;

// MMIO register offsets (virtio 1.0 / MMIO version 2).
const REG_MAGIC: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_INTERRUPT_STATUS: usize = 0x060;
const REG_INTERRUPT_ACK: usize = 0x064;
const REG_STATUS: usize = 0x070;
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DESC_HIGH: usize = 0x084;
const REG_QUEUE_DRIVER_LOW: usize = 0x090;
const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
const REG_QUEUE_DEVICE_LOW: usize = 0x0A0;
const REG_QUEUE_DEVICE_HIGH: usize = 0x0A4;
const REG_CONFIG: usize = 0x100;
// Device status bits.
const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

// Magic value: "virt" in little-endian.
const VIRTIO_MAGIC: u32 = 0x7472_6976;
// QEMU virt virtio-mmio layout.
const VIRTIO_MMIO_BASE: usize = 0x0a00_0000 + KERNEL_VA_OFFSET;
const VIRTIO_MMIO_STRIDE: usize = 0x200;
const VIRTIO_MMIO_COUNT: usize = 32;
const VIRTIO_IRQ_BASE: u32 = 48; // SPI 16 = GIC IRQ 48

// Device type IDs.
pub const DEVICE_BLK: u32 = 2;
pub const DEVICE_CONSOLE: u32 = 3;

/// A discovered virtio-mmio device.
pub struct Device {
    /// Base virtual address of the MMIO region.
    base: usize,
    /// Device type ID.
    pub device_id: u32,
    /// GIC IRQ number (for future interrupt-driven I/O).
    pub irq: u32,
    /// MMIO transport version (1 = legacy, 2 = modern).
    pub version: u32,
}

impl Device {
    fn read(&self, offset: usize) -> u32 {
        mmio::read32(self.base + offset)
    }
    fn write(&self, offset: usize, val: u32) {
        mmio::write32(self.base + offset, val);
    }

    /// Acknowledge pending interrupts (for future interrupt-driven I/O).
    pub fn ack_interrupt(&self) -> u32 {
        let status = self.read(REG_INTERRUPT_STATUS);

        self.write(REG_INTERRUPT_ACK, status);

        status
    }
    /// Read a byte from device-specific config space.
    pub fn config_read8(&self, offset: usize) -> u8 {
        mmio::read8(self.base + REG_CONFIG + offset)
    }
    /// Read a 32-bit word from device-specific config space.
    pub fn config_read32(&self, offset: usize) -> u32 {
        mmio::read32(self.base + REG_CONFIG + offset)
    }
    /// Read a 64-bit word from device-specific config space (two 32-bit reads).
    pub fn config_read64(&self, offset: usize) -> u64 {
        let lo = self.config_read32(offset) as u64;
        let hi = self.config_read32(offset + 4) as u64;

        lo | (hi << 32)
    }
    /// Signal that the driver is fully configured.
    pub fn driver_ok(&self) {
        let status = self.read(REG_STATUS);

        self.write(REG_STATUS, status | STATUS_DRIVER_OK);
    }
    /// Perform feature negotiation. Accepts no device-specific features.
    ///
    /// Returns the device's feature bits (word 0) on success.
    pub fn negotiate(&self) -> Result<u32, &'static str> {
        self.reset();
        // Acknowledge the device.
        self.write(REG_STATUS, STATUS_ACKNOWLEDGE);

        // Tell the device we know how to drive it.
        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER;

        self.write(REG_STATUS, status);
        // Read device features (word 0 only — no device-specific features needed).
        self.write(REG_DEVICE_FEATURES_SEL, 0);

        let features = self.read(REG_DEVICE_FEATURES);

        // Accept no features.
        self.write(REG_DRIVER_FEATURES_SEL, 0);
        self.write(REG_DRIVER_FEATURES, 0);
        self.write(REG_DRIVER_FEATURES_SEL, 1);
        self.write(REG_DRIVER_FEATURES, 0);

        // Set FEATURES_OK and verify the device accepted.
        let status = status | STATUS_FEATURES_OK;

        self.write(REG_STATUS, status);

        if self.read(REG_STATUS) & STATUS_FEATURES_OK == 0 {
            self.write(REG_STATUS, STATUS_FAILED);

            return Err("device rejected features");
        }

        Ok(features)
    }
    /// Notify the device that virtqueue `index` has new buffers.
    pub fn notify(&self, index: u32) {
        // Ensure all descriptor/ring writes are visible before the notify.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

        self.write(REG_QUEUE_NOTIFY, index);
    }
    /// Read the maximum queue size for virtqueue `index`.
    pub fn queue_max_size(&self, index: u32) -> u32 {
        self.write(REG_QUEUE_SEL, index);
        self.read(REG_QUEUE_NUM_MAX)
    }
    /// Reset the device (write 0 to Status register).
    pub fn reset(&self) {
        self.write(REG_STATUS, 0);
    }
    /// Configure a virtqueue: set size, physical addresses, and mark ready.
    pub fn setup_queue(&self, index: u32, size: u32, desc_pa: u64, avail_pa: u64, used_pa: u64) {
        self.write(REG_QUEUE_SEL, index);
        self.write(REG_QUEUE_NUM, size);
        self.write(REG_QUEUE_DESC_LOW, desc_pa as u32);
        self.write(REG_QUEUE_DESC_HIGH, (desc_pa >> 32) as u32);
        self.write(REG_QUEUE_DRIVER_LOW, avail_pa as u32);
        self.write(REG_QUEUE_DRIVER_HIGH, (avail_pa >> 32) as u32);
        self.write(REG_QUEUE_DEVICE_LOW, used_pa as u32);
        self.write(REG_QUEUE_DEVICE_HIGH, (used_pa >> 32) as u32);
        self.write(REG_QUEUE_READY, 1);
    }
}

/// Probe virtio-mmio devices and initialize any found drivers.
///
/// Called once during boot. Logs discovered devices and demonstrates
/// end-to-end virtqueue I/O when devices are present.
pub fn init() {
    let mut found_any = false;

    probe(|device| {
        found_any = true;

        match device.device_id {
            DEVICE_BLK => {
                uart::puts("virtio: blk\n");
                blk::demo(device);
            }
            DEVICE_CONSOLE => {
                uart::puts("virtio: console\n");
                console::demo(device);
            }
            id => {
                uart::puts("virtio: unknown id=");
                uart::put_u32(id);
                uart::puts("\n");
            }
        }
    });

    if !found_any {
        uart::puts("virtio: no devices\n");
    }
}
/// Probe all 32 virtio-mmio slots. Calls `found` for each valid device.
///
/// Supports both legacy (v1) and modern (v2) transports. QEMU `virt`
/// defaults to v1 unless `-global virtio-mmio.force-legacy=false` is set.
pub fn probe(mut found: impl FnMut(Device)) {
    for i in 0..VIRTIO_MMIO_COUNT {
        let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;

        if mmio::read32(base + REG_MAGIC) != VIRTIO_MAGIC {
            continue;
        }

        let version = mmio::read32(base + REG_VERSION);

        if version != 1 && version != 2 {
            continue;
        }

        let device_id = mmio::read32(base + REG_DEVICE_ID);

        if device_id == 0 {
            continue; // No device in this slot.
        }

        found(Device {
            base,
            device_id,
            irq: VIRTIO_IRQ_BASE + i as u32,
            version,
        });
    }
}
