// Device identity and hardware constants.

// =============================================================================
// Device identity (emulating SF600 with firmware 7.2.21, Protocol V2)
// =============================================================================

/// Response to CMD_READ_PROG_INFO (0x08).
/// Format: "SF600 V:7.2.21 S6B000001"
pub const DEVICE_STRING: &[u8] = b"SF600 V:7.2.21 S6B000001";

/// Response to CMD_READ_EEPROM (0x05): 16-byte serial ID.
/// Decoded as buf[0]<<16 | buf[1]<<8 | buf[2].
pub const SERIAL_ID: [u8; 16] = [0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

// =============================================================================
// USB descriptors
// =============================================================================

pub const USB_VID: u16 = 0x0483;
pub const USB_PID: u16 = 0xDADA;

// =============================================================================
// SPI flash command opcodes (used internally for page program / status poll)
// =============================================================================

pub const SPI_CMD_WRITE_ENABLE: u8 = 0x06;
pub const SPI_CMD_READ_STATUS: u8 = 0x05;
pub const SPI_STATUS_WIP: u8 = 0x01;

// =============================================================================
// LED bits (active-high on our hardware)
// =============================================================================

pub const LED_PASS: u8 = 0x01;
pub const LED_BUSY: u8 = 0x02;
pub const LED_ERROR: u8 = 0x04;

// =============================================================================
// Bulk transfer parameters
// =============================================================================

/// Each bulk USB transfer is 512 bytes.
pub const BULK_BLOCK_SIZE: usize = 512;

/// Each page program writes 256 bytes of real data.
pub const PAGE_SIZE: usize = 256;

/// USB max packet size for High Speed bulk endpoints (480 Mbit/s).
/// USB 2.0 spec requires wMaxPacketSize=512 for HS bulk (section 5.8.3).
pub const USB_MAX_PACKET_SIZE: u16 = 512;

/// Default SPI frequency at power-on (Hz).
/// CH32V307 SPI1 on APB2 (72 MHz); DIV_4 gives 18 MHz.
pub const DEFAULT_SPI_FREQ_HZ: u32 = 18_000_000;

// =============================================================================
// USB HS endpoint buffer count
// =============================================================================

/// Number of endpoint buffers to allocate for the USBHS driver.
/// Need at least 3: EP0 (control) + EP1 OUT (bulk) + EP2 IN (bulk).
pub const NR_EP_BUFFERS: usize = 4;
