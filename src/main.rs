#![no_std]
#![no_main]

mod config;
mod leds;
mod protocol;
mod spi_flash;
mod usb_handler;

use core::cell::RefCell;

use ch32_hal as hal;
use ch32_hal::gpio::{Level, Output};
use ch32_hal::spi::{self, Spi};
use ch32_hal::time::Hertz;
use ch32_hal::usb::EndpointDataBuffer512;
use ch32_hal::usbhs::{Driver, InterruptHandler, WakeupInterruptHandler};
use ch32_hal::{bind_interrupts, peripherals, Config};
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_sync::zerocopy_channel::Channel;
use embassy_usb::driver::{Direction, Endpoint as _, EndpointAddress, EndpointError, EndpointIn as _, EndpointOut as _};
use embassy_usb::Builder;
use panic_halt as _;
use static_cell::StaticCell;

use crate::config::*;
use crate::leds::Leds;
use crate::protocol::BulkOperation;
use crate::spi_flash::SpiFlash;
use crate::usb_handler::DediprogHandler;

// =============================================================================
// Interrupt bindings
// =============================================================================

bind_interrupts!(struct Irqs {
    USBHS => InterruptHandler<peripherals::USBHS>;
    USBHS_WKUP => WakeupInterruptHandler<peripherals::USBHS>;
});

// =============================================================================
// Shared state between USB handler (sync) and bulk worker task (async)
// =============================================================================

/// SPI flash peripheral -- borrowed by the handler for transceive (blocking)
/// and taken by the worker task for bulk operations (async with DMA).
pub static SPI_FLASH: Mutex<RefCell<Option<SpiFlash>>> =
    Mutex::new(RefCell::new(None));

/// Pending bulk operation set by the handler, consumed by the worker task.
pub static BULK_OP: Mutex<RefCell<Option<BulkOperation>>> = Mutex::new(RefCell::new(None));

/// Signal to wake the worker task when a bulk operation is ready.
pub static BULK_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

// =============================================================================
// USB device type alias
// =============================================================================

type UsbHsDriver = Driver<'static, peripherals::USBHS, NR_EP_BUFFERS, 512>;

// =============================================================================
// Entry point
// =============================================================================

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(spawner: Spawner) -> ! {
    let cfg = Config {
        rcc: hal::rcc::Config::SYSCLK_FREQ_144MHZ_HSE,
        ..Default::default()
    };
    let p = hal::init(cfg);

    hal::debug::SDIPrint::enable();
    hal::println!("DediCH32 starting up");

    // ---- SPI peripheral (async mode with DMA) ----
    let mut spi_config = spi::Config::default();
    spi_config.frequency = Hertz::hz(DEFAULT_SPI_FREQ_HZ);

    let spi = Spi::new(
        p.SPI1, p.PA5, p.PA7, p.PA6,
        p.DMA1_CH3, p.DMA1_CH2,
        spi_config,
    );
    let cs = Output::new(p.PA4, Level::High, Default::default()); // CS deasserted (high)

    // Store in shared state
    critical_section::with(|cs_tok| {
        *SPI_FLASH.borrow(cs_tok).borrow_mut() = Some(SpiFlash::new(spi, cs));
    });

    // ---- LEDs ----
    let led_pass = Output::new(p.PC0, Level::Low, Default::default());
    let led_busy = Output::new(p.PC1, Level::Low, Default::default());
    let led_error = Output::new(p.PC2, Level::Low, Default::default());
    let leds = Leds::new(led_pass, led_busy, led_error);

    // ---- USB HS driver ----
    static EP_BUFFER: StaticCell<[EndpointDataBuffer512; NR_EP_BUFFERS]> = StaticCell::new();
    let ep_buffer = EP_BUFFER.init(core::array::from_fn(|_| EndpointDataBuffer512::default()));
    let driver = Driver::new(p.USBHS, Irqs, p.PB7, p.PB6, ep_buffer);

    let mut usb_config = embassy_usb::Config::new(USB_VID, USB_PID);
    usb_config.manufacturer = Some("DediProg");
    usb_config.product = Some("SF600");
    usb_config.serial_number = Some("S6B000001");
    usb_config.max_power = 200;
    usb_config.max_packet_size_0 = 64;

    // Descriptor buffers (must be 'static)
    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 128]> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 128]),
    );

    // ---- Handler ----
    static HANDLER: StaticCell<DediprogHandler> = StaticCell::new();
    let handler = HANDLER.init(DediprogHandler::new(leds));
    builder.handler(handler);

    // ---- Vendor-class interface with bulk endpoints ----
    //
    // flashprog hard-codes: EP1 OUT (0x01) for SF600, EP2 IN (0x82) for all.
    // With USB HS, each bulk endpoint can transfer a full 512-byte block in
    // a single packet -- 8x faster than Full Speed.
    let ep1_out = EndpointAddress::from_parts(1, Direction::Out);
    let ep2_in = EndpointAddress::from_parts(2, Direction::In);

    let mut func = builder.function(0xFF, 0x00, 0x00);
    let mut iface = func.interface();
    let mut alt = iface.alt_setting(0xFF, 0x00, 0x00, None);

    let ep_out = alt.endpoint_bulk_out(Some(ep1_out), USB_MAX_PACKET_SIZE);
    let ep_in = alt.endpoint_bulk_in(Some(ep2_in), USB_MAX_PACKET_SIZE);

    drop(func); // release borrow on builder

    // ---- Build and launch ----
    let usb = builder.build();

    spawner.must_spawn(usb_device_task(usb));
    spawner.must_spawn(bulk_worker_task(ep_in, ep_out));

    hal::println!("DediCH32 ready (HS 480 Mbit/s)");

    // Main task has nothing else to do; park forever.
    loop {
        embassy_time::Timer::after_secs(3600).await;
    }
}

// =============================================================================
// USB device task -- runs the USB stack, dispatches control transfers
// =============================================================================

#[embassy_executor::task]
async fn usb_device_task(mut usb: embassy_usb::UsbDevice<'static, UsbHsDriver>) {
    usb.run().await;
}

// =============================================================================
// Bulk helpers -- split/reassemble 512-byte protocol blocks into USB packets
// =============================================================================

/// Write a full 512-byte block to the bulk IN endpoint (8 x 64-byte packets).
async fn write_bulk_block(
    ep: &mut <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointIn,
    data: &[u8; BULK_BLOCK_SIZE],
) -> Result<(), EndpointError> {
    for chunk in data.chunks(USB_MAX_PACKET_SIZE as usize) {
        ep.write(chunk).await?;
    }
    Ok(())
}

/// Read a full 512-byte block from the bulk OUT endpoint.
async fn read_bulk_block(
    ep: &mut <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointOut,
    buf: &mut [u8; BULK_BLOCK_SIZE],
) -> Result<(), EndpointError> {
    let mut offset = 0;
    while offset < BULK_BLOCK_SIZE {
        let n = ep.read(&mut buf[offset..]).await?;
        offset += n;
    }
    Ok(())
}

// =============================================================================
// Bulk worker task -- handles bulk read/write operations with double-buffered
// SPI/USB pipeline (zerocopy_channel + embassy_futures::join)
// =============================================================================

#[embassy_executor::task]
async fn bulk_worker_task(
    mut ep_in: <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointIn,
    mut ep_out: <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointOut,
) {
    loop {
        // Sleep until the USB handler signals a new bulk operation.
        BULK_SIGNAL.wait().await;

        let op = critical_section::with(|cs| BULK_OP.borrow(cs).borrow_mut().take());

        let Some(op) = op else {
            continue;
        };

        // Take the SPI peripheral out of shared state for exclusive async use.
        let flash = critical_section::with(|cs| SPI_FLASH.borrow(cs).borrow_mut().take());
        let Some(mut flash) = flash else {
            hal::println!("SPI flash not available for bulk operation");
            continue;
        };

        match op {
            BulkOperation::Read {
                address,
                block_count,
                opcode,
                addr_len,
                dummy_bytes,
            } => {
                ep_in.wait_enabled().await;
                flash
                    .start_read(opcode, address, addr_len, dummy_bytes)
                    .await;

                // Zero-copy double buffer: 2 slots of 512 bytes.
                // SPI fills one slot while USB drains the other.
                let mut buf = [[0u8; BULK_BLOCK_SIZE]; 2];
                let mut channel =
                    Channel::<CriticalSectionRawMutex, [u8; BULK_BLOCK_SIZE]>::new(&mut buf);
                let (mut sender, mut receiver) = channel.split();

                let ((), _usb_result) = embassy_futures::join::join(
                    // SPI producer: read blocks into channel slots
                    async {
                        for _i in 0..block_count {
                            let slot = sender.send().await;
                            flash.read_block(slot).await;
                            sender.send_done();
                        }
                    },
                    // USB consumer: send filled slots to the host
                    async {
                        let mut result: Result<(), EndpointError> = Ok(());
                        for _i in 0..block_count {
                            {
                                let slot = receiver.receive().await;
                                if result.is_ok() {
                                    if let Err(e) = write_bulk_block(&mut ep_in, slot).await {
                                        hal::println!("Bulk IN write error");
                                        result = Err(e);
                                    }
                                }
                            }
                            receiver.receive_done();
                        }
                        result
                    },
                )
                .await;

                flash.end_transfer();
            }

            BulkOperation::Write {
                mut address,
                block_count,
                opcode,
                addr_len,
            } => {
                ep_out.wait_enabled().await;

                // Zero-copy double buffer: USB receives into one slot
                // while SPI programs from the other.
                let mut buf = [[0u8; BULK_BLOCK_SIZE]; 2];
                let mut channel =
                    Channel::<CriticalSectionRawMutex, [u8; BULK_BLOCK_SIZE]>::new(&mut buf);
                let (mut sender, mut receiver) = channel.split();

                let (_usb_result, ()) = embassy_futures::join::join(
                    // USB producer: receive blocks from host into channel slots
                    async {
                        let mut result: Result<(), EndpointError> = Ok(());
                        for _i in 0..block_count {
                            {
                                let slot = sender.send().await;
                                if result.is_ok() {
                                    if let Err(e) = read_bulk_block(&mut ep_out, slot).await {
                                        hal::println!("Bulk OUT read error");
                                        result = Err(e);
                                    }
                                }
                            }
                            sender.send_done();
                        }
                        result
                    },
                    // SPI consumer: program pages from channel slots
                    async {
                        for _i in 0..block_count {
                            {
                                let slot = receiver.receive().await;
                                // First 256 bytes are real data; rest is padding.
                                let page_data = &slot[..PAGE_SIZE];
                                flash
                                    .write_page(opcode, address, addr_len, page_data)
                                    .await;
                            }
                            receiver.receive_done();
                            address = address.wrapping_add(PAGE_SIZE as u32);
                        }
                    },
                )
                .await;
            }
        }

        // Return the SPI peripheral to shared state so the handler can use it.
        critical_section::with(|cs| {
            *SPI_FLASH.borrow(cs).borrow_mut() = Some(flash);
        });
    }
}
