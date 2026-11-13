//! Keeps the video from outrunning the connection.
//!
//! A host that sends more than the path can carry does not lose data (the
//! connection is TCP); the excess waits in buffers along the way, and the
//! picture falls further and further behind. Measured before this existed:
//! 26 seconds behind after 40 seconds on a link slightly too slow.
//!
//! The sender cannot see those buffers, but it can see how long its pings
//! take, and pings wait in the same buffers. So the rule is about delay:
//!
//! * delay above [`HIGH_MS`]: stop sending video until the buffers have
//!   drained (delay below [`LOW_MS`]), and aim for a lower bit rate;
//! * delay below [`LOW_MS`]: raise the bit rate again: quickly back to just
//!   under the rate that last caused trouble, gently beyond it, and quickly
//!   again once the path has stayed clear for a while.
//!
//! Stopping is safe for the picture: frames are skipped *before* they are
//! encoded, so the viewer never misses a frame the next one depends on.

use std::time::{Duration, Instant};

use serde::Serialize;

/// Queueing delay above which video is held back.
pub const HIGH_MS: f64 = 300.0;
/// Queueing delay below which the path counts as clear.
pub const LOW_MS: f64 = 100.0;
/// The bit rate is never set below this.
pub const MIN_KBPS: u32 = 150;
/// A session starts at this fraction of the maximum and works up, so that a
/// slow path is found out before much has been pushed into it.
const START: f64 = 0.3;
/// Upward steps taken gently after a cut before growth speeds up.
const PATIENCE: u32 = 8;
/// How often the bit rate is reconsidered.
const STEP: Duration = Duration::from_secs(1);
/// A change smaller than this fraction is not worth telling the encoder.
const WORTH_APPLYING: f64 = 0.1;

/// What the host should do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pace {
    /// Send video. `false` while the path is draining.
    pub send: bool,
    /// The bit rate the encoder should aim for.
    pub bitrate_kbps: u32,
}

/// For the window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CongestionStatus {
    pub bitrate_kbps: u32,
    pub max_kbps: u32,
    /// Video is being held back until the connection catches up.
    pub holding: bool,
    pub delay_ms: f64,
}

pub struct Controller {
    max_kbps: u32,
    /// Fraction of `max_kbps` aimed for, between the minimum and 1.
    scale: f64,
    /// The fraction above which growth is slow: a little under the rate that
    /// last filled the buffers.
    careful_above: f64,
    holding: bool,
    /// Steps taken upwards since video was last held back.
    clear_steps: u32,
    last_step: Option<Instant>,
    applied_kbps: u32,
    delay_ms: f64,
}

impl Controller {
    pub fn new(max_kbps: u32) -> Self {
        let max_kbps = max_kbps.max(MIN_KBPS);
        let scale = START.max(f64::from(MIN_KBPS) / f64::from(max_kbps));
        Self {
            max_kbps,
            scale,
            careful_above: 1.0,
            holding: false,
            clear_steps: 0,
            last_step: None,
            applied_kbps: ((f64::from(max_kbps) * scale).round() as u32).max(MIN_KBPS),
            delay_ms: 0.0,
        }
    }

    fn floor(&self) -> f64 {
        f64::from(MIN_KBPS) / f64::from(self.max_kbps)
    }

    /// Takes the latest queueing delay and says what to do. Call it often
    /// (every frame); the bit rate itself moves at most once a second.
    pub fn update(&mut self, now: Instant, queue_delay_ms: f64) -> Pace {
        self.delay_ms = queue_delay_ms;
        // Holding switches at once, and with a gap between on and off so it
        // does not flutter.
        let was_holding = self.holding;
        if queue_delay_ms > HIGH_MS {
            self.holding = true;
        } else if queue_delay_ms < LOW_MS {
            self.holding = false;
        }

        let Some(last_step) = self.last_step else {
            // The first call only starts the clock.
            self.last_step = Some(now);
            return self.pace();
        };
        let due = now.duration_since(last_step) >= STEP;
        if self.holding && (!was_holding || due) {
            // Too much was sent. Cut, and remember where the trouble was.
            // Cutting again for each further second of holding finds a rate
            // the path can carry even when it is a small fraction.
            if was_holding {
                self.scale *= 0.5;
            } else {
                self.careful_above = (self.scale * 0.8).max(self.floor());
                self.scale *= 0.7;
                self.clear_steps = 0;
            }
            self.scale = self.scale.max(self.floor());
            self.last_step = Some(now);
        } else if !self.holding && queue_delay_ms < LOW_MS && due {
            self.clear_steps += 1;
            self.scale = if self.scale < self.careful_above {
                // Back towards the rate that worked.
                (self.scale * 1.25).min(self.careful_above)
            } else if self.clear_steps <= PATIENCE {
                // Probing for more, gently.
                self.scale * 1.05
            } else {
                // Clear for a while: the path has probably got better.
                self.scale * 1.2
            }
            .min(1.0);
            self.last_step = Some(now);
        }
        self.pace()
    }

    fn pace(&mut self) -> Pace {
        let wanted = (f64::from(self.max_kbps) * self.scale).round() as u32;
        let wanted = wanted.clamp(MIN_KBPS, self.max_kbps);
        let change = (f64::from(wanted) - f64::from(self.applied_kbps)).abs();
        if change >= f64::from(self.applied_kbps) * WORTH_APPLYING
            || wanted == self.max_kbps
            || wanted == MIN_KBPS
        {
            self.applied_kbps = wanted;
        }
        Pace {
            send: !self.holding,
            bitrate_kbps: self.applied_kbps,
        }
    }

    pub fn status(&self) -> CongestionStatus {
        CongestionStatus {
            bitrate_kbps: self.applied_kbps,
            max_kbps: self.max_kbps,
            holding: self.holding,
            delay_ms: self.delay_ms,
        }
    }
}

#[cfg(test)]
mod tests;
