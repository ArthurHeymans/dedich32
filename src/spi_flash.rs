/// SPI flash operations -- blocking (for transceive) and async (for bulk).
use ch32_hal as hal;
use ch32_hal::gpio::Output;
use ch32_hal::mode::Async;
use ch32_hal::pac::gpio::vals;
use ch32_hal::peripherals::SPI2;
use ch32_hal::spi::Spi;
use ch32_hal::time::Hertz;
use embassy_time::{Duration, Instant};

use crate::config;

// Flash bus on GPIOB: CS=PB12, SCK=PB13, MISO=PB14, MOSI=PB15.
// The SPI peripheral owns SCK/MISO/MOSI as alternate functions, but the
// GPIO mode registers stay ours to poke: the bus is Hi-Z whenever CS is
// deasserted so an in-system controller can own it.
const PIN_CS: usize = 12;
const PIN_SCK: usize = 13;
const PIN_MOSI: usize = 15;

/// Set one bus pin's GPIO mode (pins 8-15 live in CFGHR).
fn set_pin_mode(pin: usize, mode: vals::Mode, cnf: vals::Cnf) {
    hal::pac::GPIOB.cfghr().modify(|w| {
        w.set_cnf(pin % 8, cnf);
        w.set_mode(pin % 8, mode);
    });
}

pub struct SpiFlash {
    pub spi: Spi<'static, SPI2, Async>,
    pub cs: Output<'static>,
    /// While set, status reads report WIP and bulk ops stall. Covers the
    /// window where some emulators (EM100) accept erase/program commands
    /// before WIP becomes visible to an immediate status poll, so the host
    /// cannot issue the next command too early.
    force_busy_until: Option<Instant>,
}

impl SpiFlash {
    pub fn new(spi: Spi<'static, SPI2, Async>, cs: Output<'static>) -> Self {
        let mut this = Self {
            spi,
            cs,
            force_busy_until: None,
        };
        // Boot Hi-Z: never fight an in-system controller before first use.
        this.bus_idle();
        this
    }

    // =========================================================================
    // Runtime SPI clock reconfiguration
    // =========================================================================

    /// Change the SPI bus frequency using the HAL's `set_config` method.
    /// The actual clock will be the closest achievable value that does
    /// not exceed `freq_hz`, capped at the protocol max (24 MHz).
    pub fn set_frequency(&mut self, freq_hz: u32) {
        let target = freq_hz.clamp(1, config::MAX_SPI_FREQ_HZ);
        let mut cfg = self.spi.get_current_config();
        cfg.frequency = Hertz::hz(target);
        self.spi.set_config(&cfg).ok();
    }

    // =========================================================================
    // Bus idle/active control (Hi-Z when not selected)
    // =========================================================================

    /// Drive the bus: SCK/MOSI back to AF push-pull, CS to push-pull.
    /// OUTDR still holds the parked-high level, so no glitch on entry.
    fn bus_active(&mut self) {
        set_pin_mode(
            PIN_SCK,
            vals::Mode::OUTPUT_50MHZ,
            vals::Cnf::PULL_IN__AF_PUSH_PULL_OUT,
        );
        set_pin_mode(
            PIN_MOSI,
            vals::Mode::OUTPUT_50MHZ,
            vals::Cnf::PULL_IN__AF_PUSH_PULL_OUT,
        );
        set_pin_mode(
            PIN_CS,
            vals::Mode::OUTPUT_50MHZ,
            vals::Cnf::ANALOG_IN__PUSH_PULL_OUT,
        );
    }

    /// Release the bus: SCK/MOSI float, CS parked high with a pull-up so
    /// the flash never sees a spurious select. MISO is always an input.
    fn bus_idle(&mut self) {
        // Park high first (BSHR -> OUTDR), then the input's pull follows
        // OUTDR, giving pull-up rather than pull-down.
        self.cs.set_high();
        set_pin_mode(
            PIN_SCK,
            vals::Mode::INPUT,
            vals::Cnf::FLOATING_IN__OPEN_DRAIN_OUT,
        );
        set_pin_mode(
            PIN_MOSI,
            vals::Mode::INPUT,
            vals::Cnf::FLOATING_IN__OPEN_DRAIN_OUT,
        );
        set_pin_mode(
            PIN_CS,
            vals::Mode::INPUT,
            vals::Cnf::PULL_IN__AF_PUSH_PULL_OUT,
        );
    }

    // =========================================================================
    // CS control
    // =========================================================================

    #[inline]
    pub fn cs_assert(&mut self) {
        self.bus_active();
        self.cs.set_low();
    }

    #[inline]
    pub fn cs_deassert(&mut self) {
        self.bus_idle();
    }

    // =========================================================================
    // Blocking operations (called from USB handler context via critical_section)
    // =========================================================================

    /// Perform a transceive: write command bytes, then read response bytes.
    /// Used for CMD_TRANSCEIVE -- short transfers (max 16 bytes each direction).
    /// CS is asserted/deasserted within this call.
    pub fn transceive_blocking(&mut self, write_data: &[u8], read_buf: &mut [u8]) {
        // While an erase busy window is forced, report WIP for status reads
        // instead of touching the bus.
        if matches!(write_data.first(), Some(&config::SPI_CMD_READ_STATUS))
            && self
                .force_busy_until
                .is_some_and(|until| Instant::now() < until)
        {
            read_buf.fill(config::SPI_STATUS_WIP);
            return;
        }

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

        if let Some(duration) = erase_busy_duration(write_data) {
            self.force_busy_until = Some(Instant::now() + duration);
        }
    }

    // =========================================================================
    // Async operations (called from bulk worker task with DMA)
    // =========================================================================

    /// Begin a SPI read sequence: assert CS, send opcode + address + dummy bytes.
    /// After this, call `read_block()` repeatedly, then `end_transfer()`.
    pub async fn start_read(&mut self, opcode: u8, address: u32, addr_len: u8, dummy_bytes: u8) {
        self.wait_for_forced_busy_window().await;

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
        self.wait_for_forced_busy_window().await;

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
        // Some emulators do not assert WIP quickly enough for an immediate
        // status read after CS deassertion. Give the page-program operation
        // a short head start before polling so the following page's
        // WREN/program sequence is not ignored while the target is still
        // internally busy.
        embassy_time::Timer::after_micros(1_000).await;
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

    async fn wait_for_forced_busy_window(&mut self) {
        loop {
            match self.force_busy_until {
                Some(until) if Instant::now() < until => {
                    embassy_time::Timer::after_millis(1).await;
                }
                Some(_) => {
                    self.force_busy_until = None;
                    embassy_time::Timer::after_millis(25).await;
                    break;
                }
                None => break,
            }
        }
    }
}

fn erase_busy_duration(write_data: &[u8]) -> Option<Duration> {
    let opcode = *write_data.first()?;
    match opcode {
        // 4 KiB sector erase. EM100 usually finishes much sooner, but this keeps
        // the host from issuing the next command before WIP is visible.
        0x20 | 0x21 => Some(Duration::from_millis(250)),
        // 32 KiB block erase.
        0x52 | 0x5c => Some(Duration::from_millis(750)),
        // 64 KiB block erase.
        0xd8 | 0xdc => Some(Duration::from_millis(1_500)),
        // Whole-chip erase. EM100 takes long enough that immediately following
        // page-program commands can be ignored unless we hold WIP high here.
        0x60 | 0xc7 => Some(Duration::from_secs(60)),
        _ => None,
    }
}
