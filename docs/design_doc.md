# Gramslator — Design Document

## 1. Overview

Gramslator is a wearable real-time speech translation
device. It listens to spoken English through an on-board
microphone, transcribes the speech via Deepgram's streaming
API, translates the transcript via Google Translate, and
renders both the original text and the translation on a
480x320 color touchscreen — all running as `no_std` embedded
Rust firmware on an ESP32-S3 microcontroller.

Unlike bleeding-edge translation devices that synthesize
audio output via text-to-speech, Gramslator deliberately
presents translations as on-screen text. This is an
intentional design choice that explores a different
interaction model: text output lets both speakers read at
their own pace, re-read phrases they missed, and maintain
eye contact and natural conversational cadence without
waiting for a synthetic voice to finish speaking. It
preserves the original speaker's voice as the only audio in
the room, avoiding the uncanny interruption of a robotic
intermediary. The visual approach also sidesteps the
latency and cognitive load of competing audio streams —
the wearer glances down to read rather than splitting
attention between two voices.

The wearable is built on the [ELECROW CrowPanel Advance
3.5" HMI](https://a.co/d/089DIeBc)
(ESP32-S3-WROOM-1-N16R8) and uses no external computer or
phone once powered on. It connects to WiFi, authenticates
with cloud APIs over TLS, and operates continuously. Device
specs, schematics, and IC datasheets are available in the
[manufacturer's GitHub
repo](https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320).

## 2. System Architecture

### 2.1 Hardware Platform

| Component  | IC / Part              | Interface      |
|------------|------------------------|----------------|
| MCU        | ESP32-S3-WROOM-1-N16R8| —              |
| Display    | ILI9488 3.5" 480x320  | SPI @ 40 MHz   |
| Touch      | GT911 capacitive       | I2C            |
| Microphone | LMD3526B261 PDM MEMS  | I2S (PDM mode) |
| Speaker    | I2S DAC + amplifier    | I2S out        |
| Buzzer     | Piezo                  | GPIO           |
| SD Card    | —                      | SPI            |

**MCU:** Dual-core Xtensa LX7, 16 MB flash, 8 MB PSRAM

**Key GPIO assignments:**

- Display: SCK=42, MOSI=39, DC=41, CS=40, PWR=14, LED=38
- Touch: SDA=15, SCL=16, INT=47, RST=48
- Microphone: CLK=9, SD=10, EN=45
- Speaker: DOUT=12, BCLK=13, LRCLK=11, MUTE=21
- Buzzer: IO8
- SD Card: MOSI=6, MISO=4, SCK=5, CS=7

GPIO9/GPIO10 are shared between the microphone and a
wireless module via an analog switch (SGM3799) controlled
by GPIO45. The firmware routes them to the microphone at
startup.

### 2.2 Software Stack

- **Language:** Rust (edition 2024, `no_std` + `no_main`)
- **Toolchain:** Espressif Rust fork (`channel = "esp"`)
  for Xtensa target support
- **Async runtime:** Embassy on esp-rtos (cooperative
  multitasking on core 0)
- **Second core:** Blocking DMA microphone read loop on
  core 1
- **Networking:** embassy-net + smoltcp (TCP/IP),
  mbedtls-rs (TLS), edge-ws (WebSocket framing)
- **Display:** mipidsi (ILI9488 init), custom async DMA
  pixel streamer, embedded-graphics
- **Font rendering:** ttf-parser + ab_glyph_rasterizer
  (TrueType to anti-aliased bitmaps)
- **Logging:** defmt (structured binary logging over serial)

### 2.3 Data Flow

```
              Core 1 (blocking)       Core 0 (async Embassy)
             ┌──────────────┐
PDM Mic ──►  │ DMA read loop├──► MIC_PIPE ──► main_task
I2S0 DMA     └──────────────┘                    │
                                    WebSocket binary frames
                                                 ▼
                                          ┌─────────────┐
                                          │  Deepgram   │
                                          │  (cloud)    │
                                          └──────┬──────┘
                                        JSON transcript
                                                 ▼
                                          ┌─────────────┐
                                  ┌───────┤  AppState   │
                                  │       └──────┬──────┘
                                  │      TranslateSignal
                                  │              ▼
                                  │       ┌─────────────┐
                                  │       │ translation  │
                                  │       │ _task        │
                                  │       └──────┬───────┘
                                  │              │
                                  │  Google Translate (cloud)
                                  │              │
                                  │       DisplaySignal
                                  │              │
                                  ▼              ▼
                             ┌───────────────────────┐
                             │     display_task      │
                             │ ┌───────────────────┐ │
                             │ │ Translation (lg)  │ │
                             │ ├───────────────────┤ │
                             │ │ Transcript (sm)   │ │
                             │ └───────────────────┘ │
                             │ + status overlays     │
                             └───────────┬───────────┘
                                         │
                                    ILI9488 LCD
```

## 3. Module Breakdown

### 3.1 `src/app_state.rs` — Shared Application State

Central state store protected by
`critical_section::Mutex<RefCell<…>>`. Holds:

- `transcript: String` — latest Deepgram transcript
- `translation: String` — latest Google Translate result
- `wifi_status`, `deepgram_status`, `translate_status` —
  each a `ServiceStatus` enum (`Idle`, `Connecting`,
  `Connected`, `Error`)

All updates return a `bool` indicating whether the value
changed, allowing callers to avoid redundant display
refreshes. State is read as an immutable `StateSnapshot`
clone.

### 3.2 `src/elecrow_board/` — Hardware Abstraction

#### `mic/` — PDM Microphone via I2S

The ESP32-S3 HAL does not expose PDM RX directly, so the
firmware initializes I2S0 in TDM mode and then patches raw
peripheral registers to enable PDM reception with hardware
sinc decimation (div 64). This yields PCM samples at the
configured rate (8-16 kHz).

Clock derivation:
`bclk = sample_rate * 64` (the PDM clock),
`mclk = bclk * 8`.

The DMA circular buffer is read in a blocking loop on
**core 1** and pushed into a static
`embassy_sync::Pipe<_, 32000>`, bridging the blocking/async
boundary.

#### `wifi/` — WiFi Connection Management

Three spawned tasks:

1. **net_task** — embassy-net packet processing
2. **wifi_connect_task** — associates with AP (5 retries,
   10 s timeout each) + DHCP (15 s timeout)
3. **wifi_status_task** — monitors link up/down events,
   updates `ServiceStatus`

SSID and password are compiled in from `.env` via `env!()`.

#### `display/` — ILI9488 SPI Display

Initialization uses the `mipidsi` crate for the ILI9488
register sequence (Rgb666, 270 deg rotation, inversion on).
After init, the SPI bus is reclaimed and wrapped in
`AsyncDisplay`, which streams pixel data via DMA in ~4 KB
chunks.

The wire format conversion (framebuffer Rgb666 to ILI9488
shifted format: `r<<2, g<<2, b<<2`) happens inline during
flush, chunk by chunk.

#### `touch.rs` — GT911 Capacitive Touch

Polls at ~50 Hz via async I2C. Transforms touch coordinates
from the panel's portrait orientation to the display's
landscape orientation (`display_x = 479 - touch_y`,
`display_y = 319 - touch_x`). Classifies touches into
left/right zones with debounce.

### 3.3 `src/networking/` — Network Stack

#### `connection.rs` — TCP + TLS Connection Pool

A static pool of 4 buffer slots (16 KB RX + 4 KB TX each),
managed by an `AtomicU8` bitmask with compare-and-swap.
`Connection` wraps either a TLS `Session<TcpSocket>` or a
plain `TcpSocket` behind a unified `Read + Write` interface.

Connection setup: DNS resolution -> TCP connect (retry 5x)
-> optional TLS handshake -> return `Connection`.

#### `tls.rs` — TLS Initialization

Uses the ESP32-S3's hardware True Random Number Generator
(TRNG, seeded by ADC noise) to initialize mbedTLS. The
`Tls` singleton is stored in a `StaticCell` and shared
across tasks as `&'static`.

#### `deepgram.rs` — Deepgram WebSocket Client

Builds an HTTP/1.1 WebSocket upgrade request with:

- Model: `flux-general-en`
- End-of-turn detection: `eot_threshold=0.7`,
  `eot_timeout_ms=5000`
- Encoding: `linear16` at the configured sample rate
- Bearer token authentication

Supports optional TLS bypass for local testing
(`DEEPGRAM_USE_TLS` env var).

#### `websocket.rs` — WebSocket Frame Handler

Processes incoming frames: extracts transcript text from
Deepgram JSON, handles ping/pong, and signals translation
on transcript changes.

### 3.4 `src/rendering/` — Display Rendering Pipeline

#### `framebuffer.rs` — Software Framebuffer with Dirty Tracking

A 480x320 RGB666 framebuffer (~461 KB, allocated in PSRAM).
Implements `embedded_graphics::DrawTarget`. Tracks a dirty
bounding box across all pixel writes; `flush_async()`
uploads only the changed region to the hardware via DMA.

#### `font.rs` — TrueType Font Renderer

Parses a TrueType font file (Noto Sans JP — covering Latin,
Japanese, and Devanagari) from flash-mapped memory.
Rasterizes glyphs on demand using `ab_glyph_rasterizer` and
caches up to 256 entries (~512 KB) in an LRU cache.
Anti-aliased alpha blending:
`output = foreground * a + background * (1 - a)`.

#### `layout.rs` — Screen Layout and Word Wrapping

Defines the two-section layout:

- **Translation** (top ~2/3): large font (48 px), cyan text
- **Transcript** (bottom ~1/3): small font (20 px), light
  grey text

Word-wrapping splits text at word boundaries, falling back
to character-level breaks for long words. Each section
tracks scroll animation state (target offset, current
offset, ~900 px/sec linear advance).

#### `display_task.rs` — Rendering Task

Runs at ~30 FPS (33 ms frame time). On each frame:

1. Reads `StateSnapshot`
2. Re-wraps text if content changed
3. Renders status overlays (WiFi connecting, Deepgram
   connecting, translate indicator)
4. Advances scroll animations
5. Renders sections with per-section clipping
6. Flushes dirty region to hardware

When no animation is active, the task blocks on
`DisplaySignal` (zero CPU usage).

### 3.5 `src/translation/` — Translation Pipeline

#### `client.rs` — Google Translate v2 Client

Builds a JSON POST request to
`translation.googleapis.com/language/translate/v2`. Handles
chunked transfer encoding in the response. Parses the
`translatedText` field from the JSON response.

#### `translation_task.rs` — Debounced Translation Orchestrator

Waits for `TranslateSignal`, then debounces rapid transcript
updates with a 500 ms deadline. Checks a single-entry
translation cache before making network requests. On cache
miss, opens a TLS connection, translates, updates
`AppState`, and signals the display.

### 3.6 `src/flash_data.rs` — MMU Flash Mapping

Maps arbitrary flash regions into the CPU's data-bus address
space using the ESP32-S3 MMU. Scans the hardware MMU table
for free contiguous entries, configures the mapping via ROM
functions (`cache_dbus_mmu_set`), and returns a
`&'static [u8]` slice — zero-copy, no heap, random access.

Used to map the 5.5 MB TrueType font file from a dedicated
flash partition without loading it into RAM.

## 4. Key Design Decisions

### 4.1 Dual-Core Architecture

Core 0 runs the Embassy async executor (networking, display,
translation, touch). Core 1 runs a dedicated blocking DMA
read loop for the microphone. The `embassy_sync::Pipe`
bridges the two worlds — the blocking producer pushes audio
bytes, and async consumers pull them with `await`.

### 4.2 PDM Register Patching

The `esp-hal` crate (v1.0) does not support PDM RX mode.
Rather than forking the HAL, the firmware initializes I2S in
TDM mode and then patches the `I2S_RX_CONF` and
`I2S_RX_CONF1` registers directly to enable PDM with
hardware sinc decimation. This is fragile but avoids
maintaining a HAL fork.

### 4.3 Flash-Mapped Fonts

The Noto Sans JP font (5.5 MB, covering Latin + Japanese +
Devanagari) is stored in a dedicated flash partition and
mapped into the CPU address space via the MMU at runtime.
This avoids bloating the application image and enables
zero-copy random access for glyph parsing. The font
partition is flashed once; code-only changes skip it
(espflash checksums each region).

### 4.4 PSRAM-First Heap Strategy

The global allocator registers PSRAM (8 MB) first and
internal SRAM (72 KB) second. Default allocations (including
mbedTLS's internal malloc) land in PSRAM. Explicit placement
via `allocator_api` (`Box::new_in(value, InternalMemory)`)
is used when SRAM is required (e.g., atomics, which are
unreliable on PSRAM for ESP32-S3).

### 4.5 Dirty-Region Display Flushing

The framebuffer tracks a bounding box of all pixel changes
since the last flush. Only the dirty region is transmitted
to the display over SPI DMA, reducing bandwidth by 80-95%
for typical text updates. Combined with per-section
clipping, this enables smooth ~30 FPS rendering without
saturating the SPI bus.

### 4.6 Debounced Translation

Deepgram sends interim (partial) transcripts rapidly. The
translation task debounces these with a 500 ms deadline —
it waits for transcript stability before issuing a Google
Translate request. A single-entry cache avoids
re-translating identical text.

### 4.7 Connection Pool with Atomic Bitmask

Four TCP/TLS buffer slots (16 KB RX + 4 KB TX each) are
statically allocated. Slot allocation uses an `AtomicU8`
bitmask with compare-and-swap, making it safe to call from
any async task without holding a mutex across await points.

## 5. Build and Flash Workflow

### Prerequisites

- Espressif Rust toolchain: `espup install` (provides
  Xtensa compiler)
- Environment variables in `.env`:
  - `WIFI_SSID`, `WIFI_PASSWORD`
  - `DEEPGRAM_HOST`, `DEEPGRAM_TOKEN`
  - `GOOGLE_TRANSLATE_HOST`, `GOOGLE_API_KEY`
  - `SAMPLE_RATE` (8000 or 16000)

### Commands

```bash
# Build firmware (fat LTO, size-optimized)
cargo build --release

# Flash font + app to device, open serial monitor
cargo run --release
```

The `cargo run` runner invokes `flash.sh`, which:

1. Writes the font file to flash at offset `0xA00000`
2. Flashes the application binary with the custom partition
   table
3. Opens the defmt serial monitor

### Partition Layout (16 MB flash)

| Name     | Type | Offset   | Size    |
|----------|------|----------|---------|
| nvs      | data | 0x9000   | 24 KB   |
| phy_init | data | 0xF000   | 4 KB    |
| factory  | app  | 0x10000  | ~9.9 MB |
| font     | data | 0xA00000 | 6 MB    |

## 6. Dependencies

| Crate               | Purpose                          |
|----------------------|----------------------------------|
| esp-hal              | ESP32-S3 HAL (GPIO, SPI, I2S,   |
|                      | I2C, DMA)                        |
| esp-rtos             | Embassy async runtime + second   |
|                      | core support                     |
| esp-radio            | WiFi driver (smoltcp integration)|
| esp-alloc            | Heap allocator (PSRAM + SRAM)    |
| embassy-net          | Async TCP/IP networking          |
| embassy-sync         | Signals, pipes, mutexes for      |
|                      | async tasks                      |
| mbedtls-rs           | TLS 1.2 (HW-accelerated)        |
| edge-ws              | WebSocket frame encode/decode    |
| mipidsi              | MIPI DBI display controller init |
| embedded-graphics    | 2D drawing primitives            |
| ttf-parser           | TrueType font file parsing       |
| ab_glyph_rasterizer  | Glyph outline rasterization      |
| gt911                | GT911 touch controller driver    |
| defmt                | Structured binary logging        |
| smoltcp              | TCP/IP stack (used by esp-radio) |
