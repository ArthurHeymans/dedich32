use ch32_hal::peripherals::USART2;
use ch32_hal::usart::{self, UartRx, UartTx};
use dedipico_protocol::aux::*;
use embassy_futures::select::{select, select4, Either, Either4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::driver::{Endpoint as _, EndpointIn as _, EndpointOut as _};

use crate::aux::BoardGpio;
use crate::UsbHsDriver;

type AuxIn = <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointIn;
type AuxOut = <UsbHsDriver as embassy_usb::driver::Driver<'static>>::EndpointOut;

static UART_RX: Channel<CriticalSectionRawMutex, u8, 256> = Channel::new();
static UART_TX: Channel<CriticalSectionRawMutex, u8, 256> = Channel::new();
static UART_BAUD: Signal<CriticalSectionRawMutex, u32> = Signal::new();

#[embassy_executor::task]
pub async fn uart_rx_task(mut rx: UartRx<'static, USART2, ch32_hal::mode::Async>) {
    let mut byte = [0];
    loop {
        match select(rx.read(&mut byte), UART_BAUD.wait()).await {
            Either::First(Ok(())) => {
                let _ = UART_RX.try_send(byte[0]);
            }
            Either::First(Err(_)) => {} // Framing/overrun: resume receiving.
            Either::Second(baudrate) => {
                let mut config = usart::Config::default();
                config.baudrate = baudrate;
                let _ = rx.set_config(&config);
            }
        }
    }
}

#[embassy_executor::task]
pub async fn uart_tx_task(mut tx: UartTx<'static, USART2, ch32_hal::mode::Async>) {
    loop {
        let byte = UART_TX.receive().await;
        let _ = tx.write(&[byte]).await;
    }
}

#[embassy_executor::task]
pub async fn aux_task(mut ep_out: AuxOut, mut ep_in: AuxIn, mut gpio: BoardGpio<'static>) {
    let mut usb_buf = [0u8; PACKET_LEN];
    let mut uart_buf = [0u8; MAX_PAYLOAD_LEN];
    let mut uart_len = 0;
    let mut uart_deadline = None;

    loop {
        ep_out.wait_enabled().await;
        ep_in.wait_enabled().await;

        let flush_at = uart_deadline.unwrap_or(Instant::MAX);
        let pulse_at = gpio.pulse_deadline().unwrap_or(Instant::MAX);
        match select4(
            ep_out.read(&mut usb_buf),
            Timer::at(pulse_at),
            Timer::at(flush_at),
            UART_RX.receive(),
        )
        .await
        {
            Either4::First(Ok(count)) => {
                handle_packet(&mut ep_in, &mut gpio, &usb_buf[..count]).await
            }
            Either4::First(Err(_)) => {}
            Either4::Second(()) => {
                if gpio.finish_pulse_if_due() {
                    write_packet(&mut ep_in, EVT_GPIO_STATE, 0, &gpio.state()).await;
                }
            }
            Either4::Third(()) => {
                flush_uart(&mut ep_in, &mut uart_buf, &mut uart_len).await;
                uart_deadline = None;
            }
            Either4::Fourth(byte) => {
                if uart_len == 0 {
                    uart_deadline = Some(Instant::now() + Duration::from_millis(1));
                }
                uart_buf[uart_len] = byte;
                uart_len += 1;
                if uart_len == uart_buf.len() {
                    flush_uart(&mut ep_in, &mut uart_buf, &mut uart_len).await;
                    uart_deadline = None;
                }
            }
        }
    }
}

async fn flush_uart(ep_in: &mut AuxIn, buf: &mut [u8; MAX_PAYLOAD_LEN], len: &mut usize) {
    if *len != 0 {
        write_packet(ep_in, EVT_UART_DATA, 0, &buf[..*len]).await;
        *len = 0;
    }
}

async fn write_packet(ep_in: &mut AuxIn, kind: u8, request_id: u8, payload: &[u8]) {
    let mut packet = [0u8; PACKET_LEN];
    if let Some(packet) = encode(&mut packet, kind, request_id, payload) {
        let _ = ep_in.write(packet).await;
    }
}

async fn write_response(ep_in: &mut AuxIn, request_id: u8, command: u8, status: u8, data: &[u8]) {
    let data = &data[..data.len().min(MAX_PAYLOAD_LEN - 2)];
    let mut response = [0u8; MAX_PAYLOAD_LEN];
    response[0] = command;
    response[1] = status;
    response[2..2 + data.len()].copy_from_slice(data);
    write_packet(ep_in, EVT_RESPONSE, request_id, &response[..2 + data.len()]).await;
}

async fn handle_packet(ep_in: &mut AuxIn, gpio: &mut BoardGpio<'static>, packet: &[u8]) {
    let Some(data) = payload(packet) else {
        return;
    };
    let command = packet[0];
    let request_id = packet[1];
    let status = match command {
        CMD_GPIO_GET_STATE if data.is_empty() => STATUS_OK,
        CMD_GPIO_SET_DIRECTION if data.len() == 2 => {
            gpio.set_direction(data[0], data[1]);
            STATUS_OK
        }
        CMD_GPIO_SET_OUTPUT if data.len() == 2 => {
            gpio.set_output(data[0], data[1]);
            STATUS_OK
        }
        CMD_GPIO_PULSE_LOW if data.len() == 3 => {
            gpio.start_pulse_low(data[0], u16::from_le_bytes([data[1], data[2]]));
            STATUS_OK
        }
        CMD_UART_SET_BAUD if data.len() == 4 => {
            UART_BAUD.signal(
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]).clamp(300, 3_000_000),
            );
            STATUS_OK
        }
        CMD_UART_WRITE if UART_TX.free_capacity() >= data.len() => {
            for &byte in data {
                let _ = UART_TX.try_send(byte);
            }
            STATUS_OK
        }
        CMD_UART_WRITE => STATUS_BUSY,
        _ => STATUS_INVALID,
    };
    if request_id != 0 {
        let state = match command {
            CMD_GPIO_GET_STATE
            | CMD_GPIO_SET_DIRECTION
            | CMD_GPIO_SET_OUTPUT
            | CMD_GPIO_PULSE_LOW
                if status == STATUS_OK =>
            {
                Some(gpio.state())
            }
            _ => None,
        };
        write_response(
            ep_in,
            request_id,
            command,
            status,
            state.as_ref().map_or(&[], |state| state.as_slice()),
        )
        .await;
    }
}
