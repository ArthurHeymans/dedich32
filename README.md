# dedich32

Open-source firmware for the **CH32V307** RISC-V microcontroller that emulates a
**DediProg SF600** SPI flash programmer. It presents itself over USB as an SF600
(VID `0x0483`, PID `0xDADA`, firmware V7.2.21, Protocol V2) so that existing
host-side tools — specifically [flashprog](https://flashprog.org/) — can use it
to read, write, and erase SPI flash chips without requiring actual DediProg
hardware.

## Features

- **Full DediProg SF600 protocol emulation** — transceive, bulk read, bulk
  write, erase, SPI clock adjustment, LED control, chip-select, and device
  identification
- **USB 2.0 High Speed (480 Mbit/s)** — 512-byte bulk endpoints for 8x
  throughput versus Full Speed
- **Double-buffered SPI/USB pipeline** — overlaps SPI DMA transfers with USB
  transfers using async zerocopy channels for maximum throughput
- **Multiple SPI read modes** — standard, fast read, Atmel fast read, 4-byte
  address fast read
- **Configurable SPI clock** — 8 presets from 375 KHz to 24 MHz
- **LED indicators** — Pass / Busy / Error LEDs matching real SF600 behavior
- **Async DMA** for bulk transfers, blocking SPI for short transceive commands

## Hardware Requirements

A CH32V307-based board with:

| Function      | Pin(s)  |
|---------------|---------|
| SPI flash CS  | PA4     |
| SPI flash SCK | PA5     |
| SPI MISO      | PA6     |
| SPI MOSI      | PA7     |
| USB HS        | PB6/PB7 |
| Pass LED      | PC0     |
| Busy LED      | PC1     |
| Error LED     | PC2     |

A **WCH-Link** debugger is needed to flash the firmware.

## Prerequisites

- **Rust nightly** — managed automatically via `rust-toolchain.toml`
- **wlink** — WCH-Link flash tool (`cargo install wlink`)
- **ch32-hal** — must be available at `../ch32-hal` (sibling directory, local
  path dependency)

## Building

```sh
cargo build --release
```

The project uses a custom target (`riscv32imfc-unknown-none-elf.json`) and
builds `core` from source. The release profile enables LTO and optimizes for
size.

## Flashing

```sh
cargo run --release
```

This builds the firmware and flashes it via `wlink`, then opens a serial monitor
for SDI debug output.

## Usage

Once flashed, connect the board's USB HS port to your host machine. The device
enumerates as a DediProg SF600. Use `flashprog` to interact with the attached
SPI flash:

```sh
flashprog -p dediprog:dev=0 --flash-name
flashprog -p dediprog:dev=0 -r dump.bin
flashprog -p dediprog:dev=0 -w firmware.bin
```

## Examples

```sh
# LED blink test (PC0, 1 Hz)
cargo run --example blinky --release

# USB HS CDC ACM serial echo
cargo run --example usb_hs_echo --release
```

## Project Structure

```
src/
  main.rs         — entry point: SPI/USB/LED init, async task orchestration
  config.rs       — constants: USB IDs, SPI params, device identity
  protocol.rs     — DediProg command codes, SPI speed enums, packet parsing
  usb_handler.rs  — USB vendor control request handler
  spi_flash.rs    — SPI flash driver (blocking transceive + async DMA read/write)
  leds.rs         — LED control (Pass/Busy/Error on PC0/PC1/PC2)
examples/
  blinky.rs       — simple LED blink
  usb_hs_echo.rs  — USB HS CDC ACM echo
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.
