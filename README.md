# dedich32

Open-source firmware for the **CH32V307** RISC-V microcontroller that emulates a
**DediProg SF600** SPI flash programmer. It presents itself over USB as an SF600
(VID `0x0483`, PID `0xDADA`, firmware V7.2.22, Protocol V3) so that existing
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
- **Auxiliary USB interface** for a UART bridge and board reset/power-switch control

## Hardware Requirements

A CH32V307-based board with:

| Function      | Pin(s)  |
|---------------|---------|
| SPI flash CS  | PB12    |
| SPI flash SCK | PB13    |
| SPI MISO      | PB14    |
| SPI MOSI      | PB15    |
| USB HS        | PB6/PB7 |
| Pass LED      | PC0     |
| Busy LED      | PC1     |
| Error LED     | PC2     |
| UART TX / RX | PA2 / PA3 (USART2, 3.3 V TTL) |
| RESET# / POWER_SW# | PB8 / PB9 (FT pins, open-drain) |
| Board power / auxiliary state | PB10 / PB11 (FT digital inputs) |

When idle (CS deasserted) SCK/MOSI/CS are Hi-Z inputs — CS parked high
with a pull-up — so an on-board controller can own the flash bus while
the programmer is attached.

PB8–PB11 are marked FT (5 V-tolerant digital inputs) in the CH32V307
datasheet; the board-state inputs can sense 5 V digital signals, but neither
has an internal pull-up. RESET# and POWER_SW# only pull low or release.
For a 5 V pull-up on these control lines, use an external transistor/level
shifter unless the electrical design has been checked for open-drain operation;
FT is specified as an input rating. POWER_SW# is a motherboard power-button
input, not a switched power supply. UART remains 3.3 V TTL on PA2/PA3; wire
TX/RX crossed with a common ground. Verify the reference board exposes these
pins before connecting a target.

A **WCH-Link** debugger is needed to flash the firmware.

## Prerequisites

- **Rust nightly** — managed automatically via `rust-toolchain.toml`
- **probe-rs** — flashing and RTT log output (`cargo install probe-rs-tools`)
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

This builds the firmware, flashes it via `probe-rs`, and displays defmt logs
over RTT. Debug logging is enabled by default; override it for one build with,
for example, `DEFMT_LOG=info cargo run --release`. RTT logging is non-blocking,
so the firmware keeps running after the debugger is disconnected.

## Usage

Once flashed, connect the board's USB HS port to your host machine. The device
enumerates as a DediProg SF600. Use `flashprog` to interact with the attached
SPI flash:

```sh
flashprog -p dediprog:dev=0 --flash-name
flashprog -p dediprog:dev=0 -r dump.bin
flashprog -p dediprog:dev=0 -w firmware.bin
```

Interface 0 remains SF600-compatible. Interface 1 uses the same auxiliary
packet protocol as `../dedipico` (EP3 OUT and EP4 IN, 64-byte bulk packets),
so its `tools/dedipicoctl` daemon and CLI can provide a UART PTY and board
control while flashprog runs. The GPIO bit assignments are reset, power switch,
power state, and auxiliary state in that order. UART starts at 115200 baud.

```sh
(cd ../dedipico/tools/dedipicoctl && cargo run --release --target x86_64-unknown-linux-gnu -- daemon 115200)
# In another terminal, use: ... -- state | reset | power | poweroff
```

The daemon's device selection assumes only one matching DediProg-emulating
unit is attached. Keep the state inputs at valid logic levels; neither input
has an internal pull-up.

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
