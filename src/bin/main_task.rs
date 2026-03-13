use gramslator::app_state::DisplaySignal;
use gramslator::elecrow_board::mic::MIC_PIPE;
use gramslator::networking::{self as net, handle_ws_frame};
use gramslator::translation as translate;
use defmt::info;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;
use mbedtls_rs::Tls;

/// Delay before reconnecting to Deepgram after a connection error or stream
/// completion.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Persistent Deepgram streaming task
// ---------------------------------------------------------------------------

/// Persistent Embassy task that maintains a WebSocket connection to Deepgram,
/// streams microphone audio, and publishes received transcripts to the shared
/// [`AppState`](gramslator::app_state) and the translation signal.
///
/// On connection failure or stream completion the task waits briefly and
/// reconnects, running indefinitely.
#[embassy_executor::task]
pub async fn read_mic_and_send_loop_task(
    network: embassy_net::Stack<'static>,
    tls: &'static Tls<'static>,
    translate_signal: &'static translate::TranslateSignal,
    display_signal: &'static DisplaySignal,
) {
    // Wait for WiFi + DHCP before attempting any network I/O.
    network.wait_config_up().await;
    info!("Deepgram task: network is up, starting streaming loop");

    loop {
        // ---- Connect to Deepgram ----------------------------------------
        let mut conn = match net::deepgram_create_listen_socket(network, tls).await {
            Ok(c) => c,
            Err(e) => {
                info!("Deepgram connect failed: {:?}, retrying...", e);
                Timer::after(RECONNECT_DELAY).await;
                continue;
            }
        };

        // ---- Stream audio & read responses (interleaved) ----------------
        //
        // We cannot split the TLS connection into independent read/write
        // halves, so instead we interleave: after sending each audio chunk
        // we attempt a brief non-blocking recv to drain any partial
        // transcripts Deepgram has ready.

        let mask_key: u32 = 0xDEAD_BEEF; // Fixed mask key for PoC

        let mut mic_read_buf = [0u8; 8000];
        let mut recv_buf = [0u8; 4096];
        let mut done = false;
        /// How long to poll for a response between audio chunks.
        const RECV_POLL: Duration = Duration::from_millis(5);

        info!("Starting microphone streaming...");

        loop {
            if done {
                break;
            }

            let n = MIC_PIPE.read(&mut mic_read_buf[..]).await;
            info!("Mic pipe read {} bytes", n);

            // Drop duplicate channel: Data16Channel16 outputs each mono PDM
            // sample twice (L and R slots identical). Keep every other 16-bit
            // sample to produce true mono: [S0,S0,S1,S1,...] → [S0,S1,...]
            let mono_len = n / 2;
            let mut j = 0;
            for i in (0..n).step_by(4) {
                if i + 1 < n {
                    mic_read_buf[j] = mic_read_buf[i];
                    mic_read_buf[j + 1] = mic_read_buf[i + 1];
                    j += 2;
                }
            }

            if let Err(e) = edge_ws::io::send(
                &mut conn,
                edge_ws::FrameType::Binary(false),
                Some(mask_key),
                &mic_read_buf[..mono_len],
            )
            .await
            {
                info!("Failed to send audio chunk: {:?}", e);
                done = true;
                break;
            }
            if let Err(e) = conn.flush().await {
                info!("Failed to flush audio chunk: {:?}", e);
                done = true;
                break;
            }

            // Drain any responses that arrived while we were sending.
            while !done {
                match embassy_time::with_timeout(
                    RECV_POLL,
                    edge_ws::io::recv(&mut conn, &mut recv_buf),
                )
                .await
                {
                    Err(_timeout) => break, // nothing ready — send next chunk
                    Ok(Ok((frame_type, len))) => {
                        done = handle_ws_frame(
                            frame_type,
                            &recv_buf[..len],
                            &mut conn,
                            mask_key,
                            translate_signal,
                            display_signal,
                        )
                        .await;
                    }
                    Ok(Err(e)) => {
                        info!("WebSocket recv error: {:?}", e);
                        done = true;
                    }
                }
            }
        }
        // Flush translation so partial transcripts aren't stuck.
        translate_signal.signal(translate::TranscriptMessage::Flush);

        conn.close().await;
        info!(
            "Deepgram connection lost. Reconnecting in {} s...",
            RECONNECT_DELAY.as_secs()
        );
        Timer::after(RECONNECT_DELAY).await;
    }
}
