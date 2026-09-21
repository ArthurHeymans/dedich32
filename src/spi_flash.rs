/// SPI flash operations -- blocking (for transceive) and async (for bulk).
use ch32_hal::gpio::Output;
use ch32_hal::mode::Async;
use ch32_hal::peripherals::SPI1;
use ch32_hal::spi::Spi;
use ch32_hal::time::Hertz;

use crate::config;

pub struct SpiFlash {
    pub spi: Spi<'static, SPI1, Async>,
    pub cs: Output<'static>,
}

impl SpiFlash {
    pub fn new(spi: Spi<'static, SPI1, Async>, cs: Output<'static>) -> Self {
        Self { spi, cs }
    }

    // =========================================================================
    // Runtime SPI clock reconfiguration
    // =========================================================================

    /// Change the SPI bus frequency using the HAL's `set_config` method.
    /// The actual clock will be the closest achievable value that does
    /// not exceed `freq_hz`.
    pub fn set_frequency(&mut self, freq_hz: u32) {
        let mut cfg = self.spi.get_current_config();
        cfg.frequency = Hertz::hz(freq_hz);
        self.spi.set_config(&cfg).ok();
    }

    // =========================================================================
    // CS control
    // =========================================================================

    #[inline]
    pub fn cs_assert(&mut self) {
        self.cs.set_low();
    }

    #[inline]
    pub fn cs_deassert(&mut self) {
        self.cs.set_high();
    }

    // =========================================================================
    // Blocking operations (called from USB handler context via critical_section)
    // =========================================================================

    /// Perform a transceive: write command bytes, then read response bytes.
    /// Used for CMD_TRANSCEIVE -- short transfers (max 16 bytes each direction).
    /// CS is asserted/deasserted within this call.
    pub fn transceive_blocking(&mut self, write_data: &[u8], read_buf: &mut [u8]) {
        self.cs_assert();
        // Clock out the command/address bytes
        self.spi.blocking_write(write_data).ok();
        // Clock in the response bytes (MOSI sends zeros)
        if !read_buf.is_empty() {
            self.spi.blocking_read(read_buf).ok();
        }
        self.cs_deassert();
    }

    /// Write-only transceive (no read phase). CS asserted/deasserted within.
    pub fn write_only_blocking(&mut self, write_data: &[u8]) {
        self.cs_assert();
        self.spi.blocking_write(write_data).ok();
        self.cs_deassert();
    }

    // =========================================================================
    // Async operations (called from bulk worker task with DMA)
    // =========================================================================

    /// Begin a SPI read sequence: assert CS, send opcode + address + dummy bytes.
    /// After this, call `read_block()` repeatedly, then `end_transfer()`.
    pub async fn start_read(&mut self, opcode: u8, address: u32, addr_len: u8, dummy_bytes: u8) {
        self.cs_assert();

        // Build command: opcode + address + dummy
        let mut cmd = [0u8; 10]; // 1 opcode + 4 addr + 5 dummy max
        let mut len = 0;

        cmd[len] = opcode;
        len += 1;

        if addr_len == 4 {
            cmd[len] = (address >> 24) as u8;
            len += 1;
        }
        cmd[len] = (address >> 16) as u8;
        len += 1;
        cmd[len] = (address >> 8) as u8;
        len += 1;
        cmd[len] = address as u8;
        len += 1;

        for _ in 0..dummy_bytes {
            cmd[len] = 0x00;
            len += 1;
        }

        self.spi.write(&cmd[..len]).await.ok();
    }

    /// Read one block of data (up to buf.len() bytes) from the flash.
    /// CS must already be asserted via `start_read()`.
    pub async fn read_block(&mut self, buf: &mut [u8]) {
        self.spi.read(buf).await.ok();
    }

    /// Finish a multi-block transfer (deassert CS).
    pub fn end_transfer(&mut self) {
        self.cs_deassert();
    }

    /// Write one page to flash:
    ///   1. Send Write Enable (WREN, 0x06)
    ///   2. Send Page Program (opcode + address + data)
    ///   3. Poll status register until WIP clears
    pub async fn write_page(&mut self, opcode: u8, address: u32, addr_len: u8, data: &[u8]) {
        // ---- Write Enable ----
        self.cs_assert();
        self.spi.write(&[config::SPI_CMD_WRITE_ENABLE]).await.ok();
        self.cs_deassert();

        // ---- Page Program ----
        self.cs_assert();

        let mut cmd = [0u8; 5]; // opcode + up to 4 address bytes
        let mut len = 0;

        cmd[len] = opcode;
        len += 1;

        if addr_len == 4 {
            cmd[len] = (address >> 24) as u8;
            len += 1;
        }
        cmd[len] = (address >> 16) as u8;
        len += 1;
        cmd[len] = (address >> 8) as u8;
        len += 1;
        cmd[len] = address as u8;
        len += 1;

        self.spi.write(&cmd[..len]).await.ok();
        self.spi.write(data).await.ok();

        self.cs_deassert();

        // ---- Wait for completion ----
        self.poll_wip().await;
    }

    /// Poll the flash status register until the WIP (Write In Progress) bit clears.
    async fn poll_wip(&mut self) {
        loop {
            self.cs_assert();
            self.spi.write(&[config::SPI_CMD_READ_STATUS]).await.ok();
            let mut status = [0u8; 1];
            self.spi.read(&mut status).await.ok();
            self.cs_deassert();

            if status[0] & config::SPI_STATUS_WIP == 0 {
                break;
            }

            embassy_time::Timer::after_micros(50).await;
        }
    }
}
