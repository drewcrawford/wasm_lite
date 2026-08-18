// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the JavaScript `console` global.

use crate::JsValue;

crate::import! {
    "console" {
        fn raw_log(msg: &str) as "log";
        fn raw_error(msg: &str) as "error";
        fn raw_warn(msg: &str) as "warn";
        fn raw_info(msg: &str) as "info";
        fn raw_debug(msg: &str) as "debug";
        fn raw_trace(msg: &str) as "trace";
        fn raw_log_value(value: &JsValue) as "log";
    }
}

/// A JavaScript console level captured by the `diagnostics` feature.
#[cfg(feature = "diagnostics")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// One captured console call.
#[cfg(feature = "diagnostics")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleRecord {
    pub sequence: u64,
    pub level: ConsoleLevel,
    pub message: String,
}

/// A bounded slice of console history and the cursor for the next read.
#[cfg(feature = "diagnostics")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleSnapshot {
    pub records: Vec<ConsoleRecord>,
    pub next_cursor: u64,
    pub dropped: u64,
}

#[cfg(feature = "diagnostics")]
const CONSOLE_CAPACITY: usize = 256;

#[cfg(feature = "diagnostics")]
#[derive(Debug)]
struct ConsoleRing {
    records: std::collections::VecDeque<ConsoleRecord>,
    next_sequence: u64,
    capacity: usize,
}

#[cfg(feature = "diagnostics")]
impl ConsoleRing {
    fn new(capacity: usize) -> Self {
        Self {
            records: std::collections::VecDeque::with_capacity(capacity),
            next_sequence: 0,
            capacity,
        }
    }

    fn push(&mut self, level: ConsoleLevel, message: &str) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(ConsoleRecord {
            sequence: self.next_sequence,
            level,
            message: message.to_string(),
        });
        self.next_sequence = self.next_sequence.wrapping_add(1);
    }

    fn since(&self, cursor: u64) -> ConsoleSnapshot {
        let oldest = self
            .records
            .front()
            .map_or(self.next_sequence, |record| record.sequence);
        let effective_cursor = cursor.max(oldest);
        ConsoleSnapshot {
            records: self
                .records
                .iter()
                .filter(|record| record.sequence >= effective_cursor)
                .cloned()
                .collect(),
            next_cursor: self.next_sequence,
            dropped: oldest.saturating_sub(cursor),
        }
    }
}

#[cfg(feature = "diagnostics")]
struct SpinMutex<T> {
    locked: std::sync::atomic::AtomicBool,
    value: std::cell::UnsafeCell<T>,
}

#[cfg(feature = "diagnostics")]
unsafe impl<T: Send> Sync for SpinMutex<T> {}

#[cfg(feature = "diagnostics")]
impl<T> SpinMutex<T> {
    fn new(value: T) -> Self {
        Self {
            locked: std::sync::atomic::AtomicBool::new(false),
            value: std::cell::UnsafeCell::new(value),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        use std::sync::atomic::Ordering;

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        struct Unlock<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _unlock = Unlock(&self.locked);
        // SAFETY: `locked` grants this call exclusive access until `_unlock`
        // releases it, including when `f` unwinds.
        f(unsafe { &mut *self.value.get() })
    }
}

#[cfg(feature = "diagnostics")]
fn console_ring() -> &'static SpinMutex<ConsoleRing> {
    static RING: std::sync::OnceLock<SpinMutex<ConsoleRing>> = std::sync::OnceLock::new();
    RING.get_or_init(|| SpinMutex::new(ConsoleRing::new(CONSOLE_CAPACITY)))
}

#[cfg(feature = "diagnostics")]
fn capture(level: ConsoleLevel, message: &str) {
    console_ring().with(|ring| ring.push(level, message));
}

#[cfg(not(feature = "diagnostics"))]
#[inline]
fn capture(_level: (), _message: &str) {}

/// Returns captured console records at or after `cursor`.
///
/// Start with cursor `0`, then pass the returned `next_cursor` to the next call.
/// `dropped` reports records overwritten before the requested cursor could be read.
#[cfg(feature = "diagnostics")]
pub fn records_since(cursor: u64) -> ConsoleSnapshot {
    console_ring().with(|ring| ring.since(cursor))
}

macro_rules! console_string_fn {
    ($name:ident, $raw:ident, $level:ident, $docs:literal) => {
        #[doc = $docs]
        pub fn $name(msg: &str) {
            #[cfg(feature = "diagnostics")]
            capture(ConsoleLevel::$level, msg);
            #[cfg(not(feature = "diagnostics"))]
            capture((), msg);
            $raw(msg);
        }
    };
}

console_string_fn!(log, raw_log, Log, "Logs a string with `console.log`.");
console_string_fn!(
    error,
    raw_error,
    Error,
    "Logs a string with `console.error`."
);
console_string_fn!(warn, raw_warn, Warn, "Logs a string with `console.warn`.");
console_string_fn!(info, raw_info, Info, "Logs a string with `console.info`.");
console_string_fn!(
    debug,
    raw_debug,
    Debug,
    "Logs a string with `console.debug`."
);
console_string_fn!(
    trace,
    raw_trace,
    Trace,
    "Logs a string and stack trace with `console.trace`."
);

/// Logs a JavaScript value with `console.log`.
pub fn log_value(value: &JsValue) {
    #[cfg(feature = "diagnostics")]
    capture(ConsoleLevel::Log, "<JavaScript value>");
    raw_log_value(value);
}

#[cfg(all(test, feature = "diagnostics"))]
mod tests {
    use super::*;

    #[test]
    fn bounded_history_reports_a_cursor_gap() {
        let mut ring = ConsoleRing::new(2);
        ring.push(ConsoleLevel::Log, "one");
        ring.push(ConsoleLevel::Warn, "two");
        ring.push(ConsoleLevel::Error, "three");

        let snapshot = ring.since(0);
        assert_eq!(snapshot.dropped, 1);
        assert_eq!(snapshot.next_cursor, 3);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.records[0].message, "two");
        assert_eq!(snapshot.records[1].message, "three");
    }
}
