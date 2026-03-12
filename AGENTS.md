# AGENTS.md - Gramslator

## Project Overview

Gramslator is a `no_std` embedded Rust firmware for the ELECROW CrowPanel Advance
3.5" HMI ESP32-S3 touchscreen. It targets speech-to-text translation with audio
I/O, WiFi networking, and a touch display. The async runtime is Embassy on
esp-rtos.

- **Target:** `xtensa-esp32s3-none-elf` (ESP32-S3 microcontroller)
- **Rust Edition:** 2024, minimum rust-version 1.88
- **Toolchain:** ESP channel (`rust-toolchain.toml`: `channel = "esp"`)
- **Runtime:** `no_std` + `no_main`, async via Embassy executor on esp-rtos
- **Logging:** `defmt` (structured embedded logging), NOT `println!` or `log`

## Build / Run / Flash Commands

```bash
cargo build                # Debug (still opt-level="s" for embedded)
cargo build --release      # Release (fat LTO, single codegen unit, size-opt)
cargo run --release        # Flash to device + serial monitor (requires ESP32-S3)
cargo check                # Check without building (faster iteration)
cargo clippy               # Run clippy lints
cargo fmt                  # Format code
cargo fmt -- --check       # Format check (CI-friendly)
```

Runner in `.cargo/config.toml`: `espflash flash --monitor --chip esp32s3 --log-format defmt`

## Testing

No test framework configured. Would require `embedded-test` crate + linker script.
No test directories or test targets exist.

## Project Structure

```
Cargo.toml              # Manifest: dependencies, profiles, bin target
build.rs                # Linker script setup + friendly error diagnostics
partitions.csv          # ESP-IDF partition table (app + font data)
rust-toolchain.toml     # ESP Rust toolchain channel
.cargo/config.toml      # Target, runner, rustflags, build-std, env
.clippy.toml            # stack-size-threshold = 1024
.env                    # Runtime secrets (WiFi creds, API keys) - gitignored
src/
  lib.rs                # Library crate root (#![no_std] only)
  flash_data.rs         # MMU-based flash partition mapping (font data, etc.)
  bin/main.rs           # Binary entry point (async main, hardware init)
```

## Code Style Guidelines

### Crate-Level Attributes

Every binary crate must declare `#![no_std]` and `#![no_main]`. Enforce these
clippy denials at the crate level:

```rust
#![deny(clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, \
    especially those holding buffers for the duration of a data transfer.")]
#![deny(clippy::large_stack_frames)]
```

### Logging

Use `defmt` macros exclusively: `info!`, `warn!`, `error!`, `debug!`, `trace!`.
Do NOT use `println!`, `eprintln!`, or the `log` crate. The default log level is
`info` (set via `DEFMT_LOG` in `.cargo/config.toml`).

```rust
use defmt::info;
info!("Embassy initialized!");
```

### Error Handling

- Use `.expect("descriptive message")` for initialization that must succeed
  (hardware init, radio init). Failures at init are unrecoverable on embedded.
- For runtime errors in async tasks, prefer `Result` types and propagate errors
  where possible.

### Async Patterns

- Entry point: `#[esp_rtos::main] async fn main(spawner: Spawner) -> !`
- Use `embassy_time::Timer::after()` for delays, NOT busy-wait loops.
- Spawn long-running work as Embassy tasks via `spawner.spawn()`.
- Use `static_cell::StaticCell` for `'static` data required by spawned tasks.
- Use `critical_section` for shared state accessed from interrupts.

### Memory Safety

- **Stack size:** Clippy enforces `stack-size-threshold = 1024` bytes per frame.
  Keep stack allocations small; use heap (`alloc`) for larger buffers.
- **Heap:** Allocated via `esp_alloc::heap_allocator!` macro in main. The heap
  uses reclaimed RAM: `esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);`
- **No `mem::forget`:** Denied at the crate level. ESP HAL types often hold DMA
  buffers and must be properly dropped.
- **Static storage:** Use `static_cell::StaticCell` for fixed-lifetime allocations
  needed by Embassy tasks.

### Formatting

Use `cargo fmt` (rustfmt defaults) before running `cargo check` or `cargo
build`.

### Dependencies

All dependencies should enable the `defmt` feature when available for consistent
structured logging. Target-specific features must include `esp32s3`. Prefer
tilde version requirements for HAL crates (`~1.0`) to avoid breaking changes.

## Build Profiles

- **Dev:** `opt-level = "s"` -- size-optimized even in debug (debug builds too
  large/slow for embedded).
- **Release:** Fat LTO, single codegen unit, size-opt, no overflow checks, no
  debug assertions. Debug symbols kept (`debug = 2`) for defmt.

## Hardware Reference (ELECROW CrowPanel 3.5" ESP32-S3)

Key GPIO assignments for peripheral initialization:
- **I2C** (touch, RTC): SDA=IO15, SCL=IO16
- **Display** (ILI9488 SPI): SCK=IO42, SDA=IO39, RS=IO41, CS=IO40, LED=IO38, PWR=IO14
- **Touch** (GT911 via I2C): INT=IO47, RST=IO48
- **Microphone** (I2S in): EN=IO45(low), CLK=IO9, SD=IO10
- **Speaker** (I2S out): MUTE=IO21, DOUT=IO12, BCLK=IO13, LRCLK=IO11
- **SD Card** (SPI): MOSI=IO6, MISO=IO4, SCK=IO5, CS=IO7
- **Buzzer:** IO8

## Environment & Secrets

Runtime secrets are stored in `.env` (gitignored). Never commit secrets. Access
via `env!()` / `option_env!()` macros at build time.
