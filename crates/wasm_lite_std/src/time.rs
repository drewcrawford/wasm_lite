// SPDX-License-Identifier: MIT OR Apache-2.0
//! A minimal `Instant` for deadline math on wasm32.
//!
//! Replaces `web_time` (which pulls in wasm-bindgen). Backed by
//! `performance.now()` via `wasm_lite::performance`. (Native code uses
//! `std::time::Instant` directly, so this module is wasm32-only.)

use std::time::Duration;

/// A monotonic instant, in milliseconds from `performance.now()`.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub struct Instant(f64);

impl Instant {
    /// The current instant.
    pub fn now() -> Self {
        Instant(wasm_lite::performance::now())
    }

    /// Time elapsed since `earlier` (saturating at zero).
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_secs_f64(((self.0 - earlier.0).max(0.0)) / 1000.0)
    }

    /// Time elapsed since this instant.
    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }
}

impl std::ops::Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, rhs: Duration) -> Instant {
        Instant(self.0 + rhs.as_secs_f64() * 1000.0)
    }
}

impl std::ops::Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, rhs: Duration) -> Instant {
        Instant(self.0 - rhs.as_secs_f64() * 1000.0)
    }
}

impl std::ops::Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, rhs: Instant) -> Duration {
        self.duration_since(rhs)
    }
}
