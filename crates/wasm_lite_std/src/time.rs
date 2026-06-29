// SPDX-License-Identifier: MIT OR Apache-2.0
//! A cross-platform `std::time` veneer for native + wasm32.
//!
//! This mirrors the structure of the threading API: on native it re-exports the
//! real [`std::time`] types verbatim; on wasm32 it provides drop-in replacements
//! backed by the browser clocks (`performance.now()` for [`Instant`], `Date.now()`
//! for [`SystemTime`]) via `wasm_lite`. Unlike [`web_time`](https://crates.io/crates/web-time),
//! it pulls in **no** `wasm-bindgen`/`js-sys` dependency.
//!
//! [`Duration`] is re-exported unchanged on both targets.
//!
//! ```
//! use wasm_lite_std::time::{Duration, Instant};
//!
//! let start = Instant::now();
//! // ... do work ...
//! let _elapsed: Duration = start.elapsed();
//! ```
//!
//! # WASM differences from `std::time`
//!
//! - [`Instant`] resolution is whatever the browser exposes via `performance.now()`
//!   (often clamped to the millisecond for security reasons).
//! - [`SystemTime`] cannot represent instants *before* the Unix epoch; arithmetic
//!   that would move before [`UNIX_EPOCH`] returns `None`/saturates rather than
//!   producing a pre-epoch value.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use core::time::Duration;
#[cfg(target_arch = "wasm32")]
pub use wasm_impl::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use core::time::Duration;

    /// Convert a JS millisecond timestamp into a [`Duration`] from its own zero.
    ///
    /// JS clocks are non-negative and finite, so `from_secs_f64` cannot panic here.
    fn ms_to_duration(ms: f64) -> Duration {
        Duration::from_secs_f64(ms.max(0.0) / 1000.0)
    }

    /// A measurement of a monotonically nondecreasing clock, backed by
    /// `performance.now()`.
    ///
    /// Stored as a [`Duration`] from the `performance.now()` time origin so that
    /// (unlike a raw `f64`) it is `Eq`/`Ord`/`Hash`, matching [`std::time::Instant`].
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub struct Instant(Duration);

    impl Instant {
        /// Returns an instant corresponding to "now".
        pub fn now() -> Instant {
            Instant(ms_to_duration(wasm_lite::performance::now()))
        }

        /// Returns the amount of time elapsed from `earlier` to this instant,
        /// saturating to zero if `earlier` is later.
        pub fn duration_since(&self, earlier: Instant) -> Duration {
            self.saturating_duration_since(earlier)
        }

        /// Returns the amount of time elapsed from `earlier` to this instant,
        /// or `None` if `earlier` is later.
        pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
            self.0.checked_sub(earlier.0)
        }

        /// Returns the amount of time elapsed from `earlier` to this instant,
        /// saturating to zero if `earlier` is later.
        pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
            self.0.saturating_sub(earlier.0)
        }

        /// Returns the amount of time elapsed since this instant.
        pub fn elapsed(&self) -> Duration {
            Instant::now().saturating_duration_since(*self)
        }

        /// `self + duration`, or `None` on overflow.
        pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
            self.0.checked_add(duration).map(Instant)
        }

        /// `self - duration`, or `None` if the result would be before the time origin.
        pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
            self.0.checked_sub(duration).map(Instant)
        }
    }

    impl core::ops::Add<Duration> for Instant {
        type Output = Instant;
        fn add(self, other: Duration) -> Instant {
            self.checked_add(other)
                .expect("overflow when adding duration to instant")
        }
    }

    impl core::ops::AddAssign<Duration> for Instant {
        fn add_assign(&mut self, other: Duration) {
            *self = *self + other;
        }
    }

    impl core::ops::Sub<Duration> for Instant {
        type Output = Instant;
        fn sub(self, other: Duration) -> Instant {
            self.checked_sub(other)
                .expect("overflow when subtracting duration from instant")
        }
    }

    impl core::ops::SubAssign<Duration> for Instant {
        fn sub_assign(&mut self, other: Duration) {
            *self = *self - other;
        }
    }

    impl core::ops::Sub<Instant> for Instant {
        type Output = Duration;
        fn sub(self, other: Instant) -> Duration {
            self.duration_since(other)
        }
    }

    /// A measurement of the system clock (wall clock), backed by `Date.now()`.
    ///
    /// Stored as a [`Duration`] *from* [`UNIX_EPOCH`]; consequently it cannot
    /// represent instants before the epoch.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub struct SystemTime(Duration);

    /// An anchor in time which can be used to create new [`SystemTime`] instances.
    pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::ZERO);

    impl SystemTime {
        /// An anchor in time corresponding to the Unix epoch (1970-01-01 00:00 UTC).
        pub const UNIX_EPOCH: SystemTime = UNIX_EPOCH;

        /// Returns the system time corresponding to "now".
        pub fn now() -> SystemTime {
            SystemTime(ms_to_duration(wasm_lite::date::now()))
        }

        /// Returns the amount of time elapsed from `earlier` to this system time,
        /// or an error (carrying the reversed difference) if `earlier` is later.
        pub fn duration_since(&self, earlier: SystemTime) -> Result<Duration, SystemTimeError> {
            if self.0 >= earlier.0 {
                Ok(self.0 - earlier.0)
            } else {
                Err(SystemTimeError(earlier.0 - self.0))
            }
        }

        /// Returns the difference from this system time to the current time,
        /// or an error if this time is in the future.
        pub fn elapsed(&self) -> Result<Duration, SystemTimeError> {
            SystemTime::now().duration_since(*self)
        }

        /// `self + duration`, or `None` on overflow.
        pub fn checked_add(&self, duration: Duration) -> Option<SystemTime> {
            self.0.checked_add(duration).map(SystemTime)
        }

        /// `self - duration`, or `None` if the result would be before [`UNIX_EPOCH`].
        pub fn checked_sub(&self, duration: Duration) -> Option<SystemTime> {
            self.0.checked_sub(duration).map(SystemTime)
        }
    }

    impl core::ops::Add<Duration> for SystemTime {
        type Output = SystemTime;
        fn add(self, dur: Duration) -> SystemTime {
            self.checked_add(dur)
                .expect("overflow when adding duration to instant")
        }
    }

    impl core::ops::AddAssign<Duration> for SystemTime {
        fn add_assign(&mut self, other: Duration) {
            *self = *self + other;
        }
    }

    impl core::ops::Sub<Duration> for SystemTime {
        type Output = SystemTime;
        fn sub(self, dur: Duration) -> SystemTime {
            self.checked_sub(dur)
                .expect("overflow when subtracting duration from instant")
        }
    }

    impl core::ops::SubAssign<Duration> for SystemTime {
        fn sub_assign(&mut self, other: Duration) {
            *self = *self - other;
        }
    }

    /// Error returned from [`SystemTime::duration_since`] and [`SystemTime::elapsed`]
    /// when the system time is later than the reference point.
    #[derive(Clone, Debug)]
    pub struct SystemTimeError(Duration);

    impl SystemTimeError {
        /// Returns the positive duration the second time was *later* than the first.
        pub fn duration(&self) -> Duration {
            self.0
        }
    }

    impl core::fmt::Display for SystemTimeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "second time provided was later than self")
        }
    }

    impl std::error::Error for SystemTimeError {}
}
