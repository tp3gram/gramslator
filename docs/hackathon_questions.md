## Problem to solve

Language barriers prevent real-time communication between
people who speak different languages. Existing translation
apps require pulling out a phone, opening an app, and often
speaking into it one sentence at a time — breaking the flow
of natural conversation. There is no affordable, dedicated,
always-on wearable that sits between two speakers and
provides continuous live translation with both the original
transcript and the translated text visible simultaneously.

## Our solution

Gramslator is a wearable translation device built on a
[$30 ESP32-S3 touchscreen (ELECROW CrowPanel
3.5")](https://a.co/d/089DIeBc). Device specs, schematics,
and IC datasheets are available in the [manufacturer's
GitHub repo](https://github.com/Elecrow-RD/CrowPanel-Advance-3.5-HMI-ESP32-S3-AI-Powered-IPS-Touch-Screen-480x320).
It continuously listens via an on-board PDM microphone,
streams
audio to Deepgram for real-time speech-to-text transcription
over a WebSocket connection, translates the transcript via
Google Translate, and displays both the original English
transcript and the translation on a 480x320 color
touchscreen — all with no phone or computer required after
initial WiFi setup.

The entire firmware is written in Rust (`no_std`, ~3,500
lines) running on the Embassy async runtime. It uses the
ESP32-S3's dual cores: one dedicated to DMA microphone
capture, the other running async networking, translation,
and display rendering. TrueType fonts (Noto Sans JP —
Latin, Japanese, Devanagari) are stored in a dedicated flash
partition and memory-mapped for zero-copy glyph rendering
with anti-aliasing and LRU caching.

## Team members we're looking for

We are a team of three: one embedded systems engineer, one
full-stack developer, and one systems/networking engineer.
We would benefit from a UX/industrial designer to improve
the wearable form factor and touch interaction model, and a
linguist or localization specialist to help tune translation
quality and expand language support beyond the current
English-to-Spanish/Japanese/Hindi pipeline.

## Describe the level of potential impact of your project.

The core impact is demonstrating that real-time speech
translation can run on a sub-$50 microcontroller with
commodity cloud APIs. This has implications for:

- **Accessibility:** Affordable translation wearables for
  travelers, immigrants, healthcare workers, and educators
  who need instant cross-language communication without
  expensive dedicated hardware (existing products like the
  Pocketalk cost $200+).
- **Embedded Rust ecosystem:** The project pushes the
  boundaries of `no_std` Rust on ESP32-S3 — PDM microphone
  support via register patching, async TLS networking,
  TrueType font rendering with flash-mapped MMU, and
  dual-core async/blocking bridging. Several of these
  patterns are novel in the esp-hal ecosystem and could be
  contributed upstream.
- **Edge AI integration patterns:** The architecture
  demonstrates how to stream sensor data to cloud AI
  services from resource-constrained devices with graceful
  reconnection, debouncing, and status feedback — a pattern
  applicable far beyond translation.

## Describe the level of learning you/your team derived from the project.

This project pushed the team into several areas where
documentation is sparse or nonexistent:

- **ESP32-S3 PDM microphone in Rust:** The esp-hal crate
  does not support PDM RX mode. We learned to read the
  ESP32-S3 Technical Reference Manual, identify the
  I2S_RX_CONF registers, and patch them at runtime to
  enable PDM with hardware sinc decimation — a technique
  not documented anywhere in the Rust ESP ecosystem.
- **MMU flash mapping:** We learned to use the ESP32-S3's
  cache MMU to map a 5.5 MB font file from a custom flash
  partition into the CPU address space, bypassing the
  normal bootloader DROM/IROM mapping. This involved
  reading ROM function signatures from the ESP-IDF C source
  and calling them from Rust via `unsafe extern "C"`.
- **Async/blocking dual-core bridging:** We learned to
  bridge a blocking DMA read loop on core 1 with the
  Embassy async executor on core 0 using a lock-free pipe,
  handling backpressure gracefully.
- **TLS on embedded:** Getting mbedTLS working in `no_std`
  with PSRAM-backed heap allocation, hardware TRNG, and
  proper connection lifecycle management was a significant
  learning curve.
- **Anti-aliased TrueType rendering on embedded:** Building
  a glyph rasterizer pipeline (ttf-parser ->
  ab_glyph_rasterizer -> alpha-blended framebuffer) with
  LRU caching on a microcontroller, supporting CJK and
  Devanagari scripts, was entirely new to the team.
- **AI-assisted embedded development:** We used Claude
  extensively for register-level debugging, TLS
  integration, and WebSocket protocol implementation —
  learning how to effectively collaborate with an AI on
  low-level systems code.

## Describe the state of your project.

The project is a **working end-to-end prototype**. The
wearable:

- Connects to WiFi and obtains an IP via DHCP (with retry
  logic and status display)
- Captures audio from the PDM microphone via I2S DMA on
  core 1
- Streams audio to Deepgram over a TLS WebSocket connection
  in real-time
- Receives streaming transcription results and displays
  them on the bottom third of the screen
- Translates the transcript via Google Translate (with
  500 ms debouncing and single-entry caching)
- Renders the translation in large cyan text on the top
  two-thirds of the screen
- Displays connection status overlays (WiFi, Deepgram,
  Google Translate)
- Supports multi-script TrueType font rendering (Latin,
  Japanese, Devanagari) with anti-aliasing
- Auto-scrolls when text overflows the display region
- Handles disconnections gracefully with automatic
  reconnection (3-second backoff)
- Responds to touch input (left/right zone detection with
  debounce)

Remaining work for production readiness: speaker output
(I2S TX hardware is wired but not implemented in firmware),
language selection UI via touch, power management, and a
wearable enclosure.

## How much did this hackathon help your team achieve your result in the time available?

The hackathon was essential. The time pressure forced us to
make pragmatic architecture decisions early — like using
register patching instead of forking esp-hal, using a fixed
WebSocket key instead of proper nonce generation, and using
a single-entry translation cache instead of a full LRU.
These "good enough" decisions let us ship a working demo in
the available time.

The parallel workstream structure (one person on
audio/microphone, one on networking/translation, one on
display/rendering) mapped naturally to the module boundaries
in the codebase, and the hackathon format kept us focused on
integration rather than gold-plating individual components.

Having dedicated time also let us push through several
multi-hour debugging sessions (PDM register configuration,
TLS handshake failures with PSRAM, DMA buffer lifecycle
issues) that would have stalled a side project for weeks.

## Did you gain AI experience you expect to apply at work?

Yes, significantly:

- **AI-assisted low-level debugging:** We used Claude to
  help interpret ESP32-S3 register documentation, generate
  correct bit-field manipulation code, and debug I2S/PDM
  configuration issues. This pattern — using AI to bridge
  between hardware reference manuals and application code —
  is directly applicable to any embedded development work.
- **API integration patterns:** The Deepgram WebSocket
  streaming and Google Translate HTTP integration patterns
  (connection pooling, retry logic, response parsing,
  debouncing) are directly reusable for any cloud AI
  service integration.
- **Streaming AI on embedded:** We learned practical
  patterns for streaming sensor data to cloud AI models
  from microcontrollers — handling backpressure,
  reconnection, and latency constraints. This is applicable
  to any edge-to-cloud AI pipeline.
- **Claude as a pair programmer for systems code:** We
  found Claude particularly effective for `no_std` Rust —
  suggesting correct `unsafe` patterns, identifying
  lifetime issues, and generating boilerplate for embedded
  HAL traits. We expect to continue using this workflow for
  embedded and systems programming.
