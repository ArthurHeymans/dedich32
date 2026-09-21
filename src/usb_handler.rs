/// USB control request handler -- dispatches all Dediprog vendor commands.
///
/// This implements `embassy_usb::Handler` and is called synchronously from the
/// USB device task whenever a vendor control transfer arrives on EP0.
use embassy_usb::control::{InResponse, OutResponse, Request, RequestType};
use embassy_usb::Handler;

use ch32_hal as hal;

use crate::config;
use crate::leds::Leds;
use crate::protocol::*;
use crate::{BULK_OP, BULK_SIGNAL, SPI_FLASH};

pub struct DediprogHandler {
    /// Buffered read data from the last CMD_TRANSCEIVE SPI transaction.
    transceive_read_buf: [u8; 16],
    transceive_read_len: u8,

    /// LED driver.
    leds: Leds,

    /// Whether the device has been configured by the host.
    configured: bool,
}

impl DediprogHandler {
    pub fn new(leds: Leds) -> Self {
        Self {
            transceive_read_buf: [0u8; 16],
            transceive_read_len: 0,
            leds,
            configured: false,
        }
    }

    // =========================================================================
    // CMD_TRANSCEIVE (0x01) -- SPI command passthrough
    // =========================================================================

    /// OUT phase: host sends SPI command bytes. We do the full SPI transaction
    /// (write + read) immediately using blocking SPI, then buffer the result
    /// for the subsequent IN phase.
    fn cmd_transceive_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        // Protocol V2: wValue bit 0 = 1 means a read phase will follow
        let needs_read = (req.value & 0x01) != 0;

        if data.is_empty() || data.len() > 16 {
            hal::println!("TRANSCEIVE OUT: bad length {}", data.len());
            return Some(OutResponse::Rejected);
        }

        if needs_read {
            // Read up to 16 bytes back from the flash.
            let mut read_buf = [0u8; 16];
            critical_section::with(|cs| {
                if let Some(flash) = SPI_FLASH.borrow(cs).borrow_mut().as_mut() {
                    flash.transceive_blocking(data, &mut read_buf);
                }
            });
            self.transceive_read_buf = read_buf;
            self.transceive_read_len = 16;
        } else {
            // Write-only: no read phase coming.
            critical_section::with(|cs| {
                if let Some(flash) = SPI_FLASH.borrow(cs).borrow_mut().as_mut() {
                    flash.write_only_blocking(data);
                }
            });
            self.transceive_read_len = 0;
        }

        Some(OutResponse::Accepted)
    }

    /// IN phase: return the buffered SPI read data.
    fn cmd_transceive_in<'a>(&self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        let len = (req.length as usize).min(self.transceive_read_len as usize);
        buf[..len].copy_from_slice(&self.transceive_read_buf[..len]);
        Some(InResponse::Accepted(&buf[..len]))
    }

    // =========================================================================
    // CMD_READ_PROG_INFO (0x08) -- Device identification string
    // =========================================================================

    fn cmd_read_prog_info<'a>(&self, _req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        let s = config::DEVICE_STRING;
        let len = s.len().min(buf.len());
        buf[..len].copy_from_slice(&s[..len]);
        hal::println!("READ_PROG_INFO: {} bytes", len);
        Some(InResponse::Accepted(&buf[..len]))
    }

    // =========================================================================
    // CMD_READ_EEPROM (0x05) -- Serial ID
    // =========================================================================

    fn cmd_read_eeprom<'a>(&self, _req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        let id = &config::SERIAL_ID;
        let len = id.len().min(buf.len());
        buf[..len].copy_from_slice(&id[..len]);
        Some(InResponse::Accepted(&buf[..len]))
    }

    // =========================================================================
    // CMD_SET_VOLTAGE (0x0B) -- Legacy voltage read (REQTYPE_OTHER_IN)
    // =========================================================================

    fn cmd_set_voltage_legacy<'a>(
        &self,
        _req: Request,
        buf: &'a mut [u8],
    ) -> Option<InResponse<'a>> {
        if !buf.is_empty() {
            buf[0] = 0x6F;
            Some(InResponse::Accepted(&buf[..1]))
        } else {
            Some(InResponse::Rejected)
        }
    }

    // =========================================================================
    // CMD_SET_TARGET (0x04)
    // =========================================================================

    fn cmd_set_target(&self, req: Request) -> Option<OutResponse> {
        hal::println!("SET_TARGET: {}", req.value);
        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_SET_VCC (0x09)
    // =========================================================================

    fn cmd_set_vcc(&self, req: Request) -> Option<OutResponse> {
        // No voltage switching hardware; the rail is always on. Accept and log.
        hal::println!("SET_VCC: {} (no-op)", req.value);
        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_SET_SPI_CLK (0x61)
    // =========================================================================

    fn cmd_set_spi_clk(&self, req: Request) -> Option<OutResponse> {
        let speed = SpiSpeed::from_code(req.value);
        let freq = speed.frequency_hz();
        hal::println!("SET_SPI_CLK: {} Hz (code {})", freq, req.value);
        critical_section::with(|cs| {
            if let Some(flash) = SPI_FLASH.borrow(cs).borrow_mut().as_mut() {
                flash.set_frequency(freq);
            }
        });
        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_SET_IO_LED (0x07)
    // =========================================================================

    fn cmd_set_io_led(&mut self, req: Request) -> Option<OutResponse> {
        self.leds.set_from_wvalue(req.value);
        hal::println!("SET_IO_LED: wValue=0x{:04x}", req.value);
        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_SET_STANDALONE (0x0A) -- ACK and ignore
    // =========================================================================

    fn cmd_set_standalone(&self, req: Request) -> Option<OutResponse> {
        hal::println!("SET_STANDALONE: wValue={}", req.value);
        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_IO_MODE (0x15)
    // =========================================================================

    fn cmd_io_mode(&self, req: Request) -> Option<OutResponse> {
        // Single-lane hardware only. Reject dual/quad so the host fails fast
        // instead of clocking multi-lane opcodes out single-lane (corrupt).
        if req.value == 0 {
            Some(OutResponse::Accepted)
        } else {
            hal::println!("IO_MODE: reject multi-lane value {}", req.value);
            Some(OutResponse::Rejected)
        }
    }

    // =========================================================================
    // CMD_READ (0x20) -- Bulk read setup
    // =========================================================================

    fn cmd_read_setup(&self, _req: Request, data: &[u8]) -> Option<OutResponse> {
        let setup = match parse_read_setup(data) {
            Some(v) => v,
            None => {
                hal::println!("READ setup: bad packet (len={})", data.len());
                return Some(OutResponse::Rejected);
            }
        };

        // Single-lane hardware: 8 clocks per dummy byte.
        let dummy_bytes = setup.dummy_cycles.div_ceil(8);

        hal::println!(
            "READ setup: addr=0x{:08x} blocks={} opcode=0x{:02x} mode={} addr_len={} dummy={}B",
            setup.address,
            setup.block_count,
            setup.opcode,
            setup.mode_byte,
            setup.addr_len,
            dummy_bytes
        );

        let op = BulkOperation::Read {
            address: setup.address,
            block_count: setup.block_count,
            opcode: setup.opcode,
            addr_len: setup.addr_len,
            dummy_bytes,
        };
        critical_section::with(|cs| {
            *BULK_OP.borrow(cs).borrow_mut() = Some(op);
        });
        BULK_SIGNAL.signal(());

        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // CMD_WRITE (0x30) -- Bulk write setup
    // =========================================================================

    fn cmd_write_setup(&self, _req: Request, data: &[u8]) -> Option<OutResponse> {
        let (block_count, mode_byte, opcode, address) = match parse_rw_cmd_v2(data) {
            Some(v) => v,
            None => {
                hal::println!("WRITE setup: bad packet (len={})", data.len());
                return Some(OutResponse::Rejected);
            }
        };

        let write_mode = WriteMode::from_byte(mode_byte);
        let addr_len = match write_mode {
            Some(mode) if mode.uses_4byte_addr() => 4u8,
            _ => 3u8,
        };

        let actual_opcode = if opcode != 0 { opcode } else { 0x02 };

        hal::println!(
            "WRITE setup: addr=0x{:08x} blocks={} opcode=0x{:02x} mode={} addr_len={}",
            address,
            block_count,
            actual_opcode,
            mode_byte,
            addr_len
        );

        let op = BulkOperation::Write {
            address,
            block_count,
            opcode: actual_opcode,
            addr_len,
        };
        critical_section::with(|cs| {
            *BULK_OP.borrow(cs).borrow_mut() = Some(op);
        });
        BULK_SIGNAL.signal(());

        Some(OutResponse::Accepted)
    }

    // =========================================================================
    // Stub commands
    // =========================================================================

    fn cmd_get_button<'a>(&self, _req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if !buf.is_empty() {
            buf[0] = 0x01; // button not pressed
            Some(InResponse::Accepted(&buf[..1]))
        } else {
            Some(InResponse::Rejected)
        }
    }

    fn cmd_check_socket<'a>(&self, _req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if !buf.is_empty() {
            buf[0] = 0x00;
            Some(InResponse::Accepted(&buf[..1]))
        } else {
            Some(InResponse::Accepted(&buf[..0]))
        }
    }
}

// =============================================================================
// embassy_usb::Handler implementation
// =============================================================================

impl Handler for DediprogHandler {
    fn configured(&mut self, configured: bool) {
        self.configured = configured;
        if configured {
            hal::println!("USB configured");
        } else {
            hal::println!("USB deconfigured");
        }
    }

    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if req.request_type != RequestType::Vendor {
            return None;
        }

        match req.request {
            CMD_TRANSCEIVE => self.cmd_transceive_out(req, data),
            CMD_SET_VPP => Some(OutResponse::Accepted),
            CMD_SET_TARGET => self.cmd_set_target(req),
            CMD_SET_IO_LED => self.cmd_set_io_led(req),
            CMD_SET_VCC => self.cmd_set_vcc(req),
            CMD_SET_STANDALONE => self.cmd_set_standalone(req),
            CMD_IO_MODE => self.cmd_io_mode(req),
            CMD_SET_CS => {
                hal::println!("SET_CS: wValue={}", req.value);
                critical_section::with(|cs| {
                    if let Some(flash) = SPI_FLASH.borrow(cs).borrow_mut().as_mut() {
                        if req.value == 0 {
                            flash.cs_assert();
                        } else {
                            flash.cs_deassert();
                        }
                    }
                });
                Some(OutResponse::Accepted)
            }
            CMD_SET_HOLD => {
                hal::println!("SET_HOLD: ACK");
                Some(OutResponse::Accepted)
            }
            CMD_READ => self.cmd_read_setup(req, data),
            CMD_WRITE => self.cmd_write_setup(req, data),
            CMD_SET_SPI_CLK => self.cmd_set_spi_clk(req),
            _ => {
                hal::println!("Unknown OUT cmd 0x{:02x}, ACK", req.request);
                Some(OutResponse::Accepted)
            }
        }
    }

    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if req.request_type != RequestType::Vendor {
            return None;
        }

        match req.request {
            CMD_TRANSCEIVE => self.cmd_transceive_in(req, buf),
            CMD_READ_EEPROM => self.cmd_read_eeprom(req, buf),
            CMD_READ_PROG_INFO => self.cmd_read_prog_info(req, buf),
            CMD_SET_VOLTAGE => self.cmd_set_voltage_legacy(req, buf),
            CMD_GET_BUTTON => self.cmd_get_button(req, buf),
            CMD_GET_UID => {
                let uid: [u8; 8] = [0xDE, 0xD1, 0x01, 0xC0, 0x00, 0x00, 0x00, 0x01];
                let len = uid.len().min(buf.len());
                buf[..len].copy_from_slice(&uid[..len]);
                Some(InResponse::Accepted(&buf[..len]))
            }
            CMD_READ_FPGA_VERSION => {
                let ver: [u8; 2] = [0x00, 0x01];
                let len = ver.len().min(buf.len()).min(req.length as usize);
                buf[..len].copy_from_slice(&ver[..len]);
                Some(InResponse::Accepted(&buf[..len]))
            }
            CMD_CHECK_SOCKET => self.cmd_check_socket(req, buf),
            _ => {
                hal::println!("Unknown IN cmd 0x{:02x}, reject", req.request);
                None
            }
        }
    }
}
