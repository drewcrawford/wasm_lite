// SPDX-License-Identifier: MIT OR Apache-2.0
//! Notice that the process was asked to quit, so browser teardown can still run.
//!
//! The runner closes its WebDriver session and kills its driver from `Drop`,
//! which a signal never runs: a killed runner leaves the driver and a headless
//! browser behind, still executing whatever the test was doing. A spin-wait
//! test then burns a core for as long as the machine stays up.
//!
//! A signal handler may not make HTTP calls, so this one only records the
//! signal number. [`pending`] hands it to an ordinary thread — see
//! `webdriver::arm` — which can do the real work.

use std::sync::atomic::{AtomicI32, Ordering};

/// The signal that arrived, or `0` if none has.
static PENDING: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
const SIGHUP: i32 = 1;
#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;

#[cfg(unix)]
unsafe extern "C" {
    /// `signal(2)`. The handler is passed as `usize` because the C type is a
    /// union of a function pointer with the `SIG_DFL`/`SIG_IGN` sentinels.
    fn signal(signum: i32, handler: usize) -> usize;
}

/// Record the signal. Storing to an atomic is async-signal-safe; nothing else
/// here would be.
#[cfg(unix)]
extern "C" fn handle(sig: i32) {
    PENDING.store(sig, Ordering::SeqCst);
}

/// Take over the signals that a Ctrl-C, a `kill`, or a closed terminal sends.
///
/// Only ever call this alongside something that will act on [`pending`] —
/// installing a handler suppresses the default "die now" behavior, so a
/// caller that then ignores the flag would leave the process unkillable by
/// Ctrl-C.
#[cfg(unix)]
pub fn install() {
    for sig in [SIGHUP, SIGINT, SIGTERM] {
        // SAFETY: `handle` is a plain `extern "C"` function of the right
        // signature, and the return value (the previous handler) is discarded
        // because nothing here chains to it.
        unsafe { signal(sig, handle as *const () as usize) };
    }
}

/// Non-unix platforms keep their default behavior; there is no cleanup window.
#[cfg(not(unix))]
pub fn install() {}

/// The signal that arrived, if one has.
pub fn pending() -> Option<i32> {
    match PENDING.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}
