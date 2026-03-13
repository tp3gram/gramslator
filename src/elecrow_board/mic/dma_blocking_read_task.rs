extern crate alloc;

use defmt::{error, info};
use esp_hal::Blocking;
use esp_hal::i2s::master::I2sRx;

use super::{DMA_BUF_SIZE, MIC_PIPE};

/// Starts circular DMA and runs a blocking read loop, pushing popped audio
/// data into [`MIC_PIPE`]. Does not return unless a DMA error occurs.
pub fn read_mic_dma_loop_blocking(
    i2s_rx: &mut I2sRx<'_, Blocking>,
    rx_buffer: &mut [u8; DMA_BUF_SIZE],
) {
    info!("Mic DMA");

    let mut transfer = i2s_rx
        .read_dma_circular(rx_buffer)
        .expect("Failed to start I2S circular DMA read");

    let mut buf = alloc::vec![0u8; DMA_BUF_SIZE];

    loop {
        match transfer.available() {
            Err(e) => {
                info!("DMA error: {}", e);
                break;
            }
            Ok(0) => {} // nothing ready yet
            Ok(_) => {
                let read = transfer.pop(&mut buf).expect("pop failed");

                // Non-blocking write into the pipe; drops data if the pipe is full.
                let _ = MIC_PIPE.try_write(&buf[..read]).map_err(|e| {
                    error!("Pipe write error: {}", e);
                });
            }
        }
    }

    info!("DMA loop exited.");
}
