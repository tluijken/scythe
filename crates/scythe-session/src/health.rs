//! Keeping a live rig going: the audio device going away and coming back, and how the audio
//! holds up (xruns, DSP load) for a status display.

use std::time::{Duration, Instant};

use scythe_engine::{DeviceEvent, Health};

use crate::Session;

/// How long the highest DSP load is held for reports.
const PEAK_WINDOW: Duration = Duration::from_secs(2);

impl Session {
    /// Follows the engine's device watch: tells the user when the device is lost (the session
    /// plays on silently) and reopens the inputs when it is back.
    pub(crate) fn watch_device(&mut self) {
        self.retry_busy_device();
        match self.engine.maintain() {
            Some(DeviceEvent::Lost(reason)) => {
                self.notify(format!("Audio device lost: {reason}. Playing on silently until it is back"));
            }
            Some(DeviceEvent::Restored(name)) => {
                self.notify(format!("Audio device {name} is back"));
                self.sync();
            }
            None => {}
        }
        if self.load_window.1.elapsed() >= PEAK_WINDOW {
            self.load_window = (self.engine.take_load_peak(), Instant::now());
        }
    }

    /// The device and how the audio is doing; the load peak covers the last few seconds.
    pub fn health(&self) -> Health {
        let mut health = self.engine.health();
        health.load_peak = health.load_peak.max(self.load_window.0);
        health
    }
}
