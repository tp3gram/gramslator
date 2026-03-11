#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

mod elecrow_board;

use defmt::info;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::timer::timg::TimerGroup;
use mbedtls_rs::Tls;
use static_cell::StaticCell;
use tinyrlibc as _;

extern crate alloc;

/// Raw WAV file baked into flash. The PCM data starts at byte 44 (standard WAV header).
const AUDIO_WAV: &[u8] = include_bytes!("assets/missile.wav");
const WAV_HEADER_SIZE: usize = 44;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // mbedTLS alone needs 40+ KB for session state, so we use regular SRAM.
    esp_alloc::heap_allocator!(size: 150_000);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // True Random Number Generator — needs ADC1 as entropy source
    let _trng_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    static TRNG: StaticCell<Trng> = StaticCell::new();
    let trng = TRNG.init(Trng::try_new().expect("TrngSource not active"));

    // Create mbedtls-rs TLS instance (singleton — needs &'static mut Trng)
    let mut tls = Tls::new(trng).expect("Failed to create TLS instance");
    tls.set_debug(1);

    info!("Embassy initialized!");

    // ---- WiFi -----------------------------------------------------------------

    let stack = elecrow_board::network::init(
        elecrow_board::network::NetworkHardware {
            wifi: peripherals.WIFI,
        },
        &spawner,
    )
    .await;

    // ---- TLS connection -------------------------------------------------------

    let mut conn =
        elecrow_board::network::TlsConnection::init(stack, env!("DEEPGRAM_HOST"), 443, &tls)
            .await
            .expect("Failed to establish TLS connection");

    // ---- WebSocket upgrade ----------------------------------------------------

    elecrow_board::network::websocket_upgrade(&mut *conn).await;

    // ---- Stream audio ---------------------------------------------------------

    let audio_data = &AUDIO_WAV[WAV_HEADER_SIZE..];
    let chunk_size = 2048;
    let mask_key: u32 = 0xDEAD_BEEF; // Fixed mask key for PoC

    info!(
        "Sending {} bytes of audio ({} chunks)...",
        audio_data.len(),
        audio_data.len().div_ceil(chunk_size)
    );

    for (i, chunk) in audio_data.chunks(chunk_size).enumerate() {
        edge_ws::io::send(
            &mut *conn,
            edge_ws::FrameType::Binary(false),
            Some(mask_key),
            chunk,
        )
        .await
        .expect("Failed to send audio chunk");
        conn.flush().await.expect("Failed to flush audio chunk");

        if i % 10 == 0 {
            info!("  Sent chunk {}", i);
        }
    }
    info!("Audio sent! Keeping connection open for 10 seconds...");

    // ---- Read responses for 10 seconds ----------------------------------------

    let deadline = embassy_time::Instant::now() + Duration::from_secs(10);
    let mut recv_buf = [0u8; 4096];
    let mut done = false;

    while !done && embassy_time::Instant::now() < deadline {
        let remaining = deadline - embassy_time::Instant::now();

        match embassy_time::with_timeout(
            remaining,
            edge_ws::io::recv(&mut *conn, &mut recv_buf),
        )
        .await
        {
            Err(_timeout) => {
                info!("10-second window elapsed.");
                break;
            }
            Ok(Ok((frame_type, len))) => match frame_type {
                edge_ws::FrameType::Text(_) => {
                    let text = core::str::from_utf8(&recv_buf[..len]).unwrap_or("<invalid UTF-8>");
                    info!("Received: {}", text);
                }
                edge_ws::FrameType::Binary(_) => {
                    info!("Received binary frame ({} bytes)", len);
                }
                edge_ws::FrameType::Close => {
                    info!("WebSocket closed by server.");
                    done = true;
                }
                edge_ws::FrameType::Ping => {
                    info!("Ping received, sending pong");
                    let _ = edge_ws::io::send(
                        &mut *conn,
                        edge_ws::FrameType::Pong,
                        Some(mask_key),
                        &recv_buf[..len],
                    )
                    .await;
                    let _ = conn.flush().await;
                }
                other => {
                    info!("Received {:?} frame ({} bytes)", other, len);
                }
            },
            Ok(Err(e)) => {
                info!("WebSocket recv error: {:?}", e);
                done = true;
            }
        }
    }

    // Signal end of audio stream
    if !done {
        let close_stream = b"{\"type\":\"CloseStream\"}";
        edge_ws::io::send(
            &mut *conn,
            edge_ws::FrameType::Text(false),
            Some(mask_key),
            close_stream,
        )
        .await
        .expect("Failed to send CloseStream");
        conn.flush().await.expect("Failed to flush CloseStream");
        info!("Sent CloseStream");
    }

    info!("Done! Deepgram streaming complete.");

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
