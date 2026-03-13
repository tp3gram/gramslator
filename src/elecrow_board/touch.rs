//! GT911 capacitive touch controller driver for the ELECROW CrowPanel 3.5" HMI.
//!
//! The GT911 is connected via I2C (SDA=GPIO15, SCL=GPIO16) with an active-low
//! reset on GPIO48.  This module initialises the controller and provides an
//! Embassy task that polls for single-touch events, classifying each touch as
//! either "left" or "right" based on the display midpoint.
//!
//! ## Coordinate transform
//!
//! The display is rotated 270° CCW + vertically flipped relative to the panel's
//! native 320×480 portrait orientation.  The GT911 reports in native panel
//! coordinates, so the mapping is:
//!
//! - **Display X** (0..480, horizontal) = **479 − Touch Y**
//! - **Display Y** (0..320, vertical)   = **319 − Touch X**

use defmt::{info, warn};
use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::peripherals::{GPIO15, GPIO16, GPIO48, I2C0};
use esp_hal::time::Rate;
use gt911::Gt911;

/// Display width in pixels (after 270° rotation).
const DISPLAY_WIDTH: u16 = 480;

/// Touch panel native width in pixels (portrait, before rotation).
const TOUCH_PANEL_WIDTH: u16 = 320;

/// Display-X threshold for zone split: left is [0, MID), right is [MID, 480).
const ZONE_MID_X: u16 = DISPLAY_WIDTH / 2;

/// Touch zone identifier for debounce tracking.
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Left,
    Right,
}

// ---------------------------------------------------------------------------
// Hardware descriptor
// ---------------------------------------------------------------------------

pub struct TouchHardware<'a> {
    pub i2c: I2C0<'a>,
    pub sda: GPIO15<'a>,
    pub scl: GPIO16<'a>,
    pub rst: GPIO48<'a>,
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Initialise the GT911 touch controller and return the async I2C bus.
///
/// Performs the hardware reset sequence, then verifies the product-ID over I2C.
pub fn init(hw: TouchHardware<'_>) -> I2c<'_, esp_hal::Async> {
    // -- Reset the GT911 (active-low, hold ≥10 ms) -------------------------
    let mut rst = Output::new(hw.rst, Level::Low, OutputConfig::default());
    let delay = Delay::new();
    delay.delay_millis(20);
    rst.set_high();
    delay.delay_millis(50); // datasheet: wait ≥50 ms after reset release

    // RST pin is no longer needed — the GT911 latches its I2C address (0x5D)
    // on the rising edge of RST when INT is low (default).
    core::mem::drop(rst);

    // -- I2C bus (400 kHz, async) -------------------------------------------
    let i2c = I2c::new(
        hw.i2c,
        Config::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("I2C0 config")
    .with_sda(hw.sda)
    .with_scl(hw.scl)
    .into_async();

    info!("GT911 touch controller reset complete, I2C bus ready");
    i2c
}

// ---------------------------------------------------------------------------
// Embassy task
// ---------------------------------------------------------------------------

/// Long-running Embassy task that polls the GT911 for touch events.
///
/// Each detected touch is classified into a **left** or **right** zone based
/// on the X coordinate relative to [`ZONE_MID_X`] and logged via `defmt`.
#[embassy_executor::task]
pub async fn touch_task(mut i2c: I2c<'static, esp_hal::Async>) {
    let gt = Gt911::default();
    let mut buf = [0u8; 8]; // GET_TOUCH_BUF_SIZE

    // GT911 init — retry a few times in case the controller isn't ready yet.
    for attempt in 0..5 {
        match gt.init(&mut i2c, &mut buf).await {
            Ok(()) => {
                info!("GT911 initialised (attempt {})", attempt + 1);
                break;
            }
            Err(_) if attempt < 4 => {
                warn!("GT911 init attempt {} failed, retrying…", attempt + 1);
                Timer::after(Duration::from_millis(100)).await;
            }
            Err(_) => {
                warn!("GT911 init failed after 5 attempts — touch disabled");
                // Park forever; we can't do anything without a working controller.
                loop {
                    Timer::after(Duration::from_secs(60)).await;
                }
            }
        }
    }

    // -- Poll loop (with debounce) -----------------------------------------
    // Only log once per finger-down event; reset when the finger lifts.
    let mut active_zone: Option<Zone> = None;

    loop {
        match gt.get_touch(&mut i2c, &mut buf).await {
            Ok(Some(point)) => {
                // Map native touch coords → rotated display coords.
                let display_x = (DISPLAY_WIDTH - 1) - point.y;
                let display_y = (TOUCH_PANEL_WIDTH - 1) - point.x;

                let zone = if display_x < ZONE_MID_X {
                    Zone::Left
                } else {
                    Zone::Right
                };

                // Only log on initial contact or when sliding into the other zone.
                if active_zone != Some(zone) {
                    match zone {
                        Zone::Left => info!(
                            "Touch LEFT  — display=({}, {}), raw=({}, {}), area={}",
                            display_x, display_y, point.x, point.y, point.area
                        ),
                        Zone::Right => info!(
                            "Touch RIGHT — display=({}, {}), raw=({}, {}), area={}",
                            display_x, display_y, point.x, point.y, point.area
                        ),
                    }
                    active_zone = Some(zone);
                }
            }
            Ok(None) => {
                // Finger lifted — reset debounce so the next touch fires again.
                active_zone = None;
            }
            Err(_) => {
                // NotReady — no new data since last poll, perfectly normal.
            }
        }

        // ~50 Hz polling rate — fast enough for responsive touch, light on CPU.
        Timer::after(Duration::from_millis(20)).await;
    }
}
