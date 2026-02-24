//! Simple LED blink example for CH32V307.
//!
//! Blinks an LED on PC0 at 1 Hz using embassy async tasks.
//!
//! Run with: cargo run --example blinky --release

#![no_std]
#![no_main]

use ch32_hal as hal;
use ch32_hal::gpio::{AnyPin, Level, Output};
use ch32_hal::Peri;
use embassy_executor::Spawner;
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::task]
async fn blink(pin: Peri<'static, AnyPin>, interval_ms: u64) {
    let mut led = Output::new(pin, Level::Low, Default::default());
    loop {
        led.set_high();
        Timer::after_millis(interval_ms).await;
        led.set_low();
        Timer::after_millis(interval_ms).await;
    }
}

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(spawner: Spawner) -> ! {
    let cfg = hal::Config {
        rcc: hal::rcc::Config::SYSCLK_FREQ_144MHZ_HSI,
        ..Default::default()
    };
    let p = hal::init(cfg);

    hal::debug::SDIPrint::enable();
    hal::println!("Blinky example starting");

    spawner.must_spawn(blink(p.PC0.into(), 500));

    loop {
        Timer::after_secs(1).await;
        hal::println!("tick");
    }
}
