//! USB High Speed CDC ACM serial echo example for CH32V307.
//!
//! Enumerates as a USB serial device at 480 Mbit/s and echoes received data.
//! Use a terminal emulator (minicom, screen, etc.) to test.
//!
//! Run with: cargo run --example usb_hs_echo --release

#![no_std]
#![no_main]

use ch32_hal as hal;
use ch32_hal::usb::EndpointDataBuffer512;
use ch32_hal::usbhs::{Driver, Instance, InterruptHandler, WakeupInterruptHandler};
use ch32_hal::{bind_interrupts, peripherals, Config};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::Builder;
use panic_halt as _;

bind_interrupts!(struct Irqs {
    USBHS => InterruptHandler<peripherals::USBHS>;
    USBHS_WKUP => WakeupInterruptHandler<peripherals::USBHS>;
});

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(_spawner: Spawner) {
    let cfg = Config {
        rcc: hal::rcc::Config::SYSCLK_FREQ_144MHZ_HSI,
        ..Default::default()
    };
    let p = hal::init(cfg);

    hal::debug::SDIPrint::enable();
    hal::println!("USB HS Echo example starting");

    // Create the USB HS driver with 512-byte endpoint buffers.
    let mut buffer: [EndpointDataBuffer512; 4] =
        core::array::from_fn(|_| EndpointDataBuffer512::default());
    let driver = Driver::new(p.USBHS, Irqs, p.PB7, p.PB6, &mut buffer);

    // Create embassy-usb config.
    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("DediCH32");
    config.product = Some("USB-HS Serial Echo");
    config.serial_number = Some("12345678");

    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );

    // Create CDC ACM class with 512-byte max packet size (USB HS).
    let mut class = CdcAcmClass::new(&mut builder, &mut state, 512);
    let mut usb = builder.build();

    let usb_fut = usb.run();

    let echo_fut = async {
        loop {
            class.wait_connection().await;
            hal::println!("USB connected");
            let _ = echo(&mut class).await;
            hal::println!("USB disconnected");
        }
    };

    join(usb_fut, echo_fut).await;
}

struct Disconnected {}

impl From<EndpointError> for Disconnected {
    fn from(val: EndpointError) -> Self {
        match val {
            EndpointError::BufferOverflow => panic!("Buffer overflow"),
            EndpointError::Disabled => Disconnected {},
        }
    }
}

async fn echo<'d, T: Instance + 'd, const NR_EP: usize, const SIZE: usize>(
    class: &mut CdcAcmClass<'d, Driver<'d, T, NR_EP, SIZE>>,
) -> Result<(), Disconnected> {
    let mut buf = [0; 512];
    loop {
        let n = class.read_packet(&mut buf).await?;
        let data = &buf[..n];
        class.write_packet(data).await?;
    }
}
