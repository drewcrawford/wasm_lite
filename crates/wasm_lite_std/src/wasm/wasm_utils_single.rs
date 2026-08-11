// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stable wasm utilities for builds without shared memory or atomics.

use std::sync::atomic::{AtomicI32, Ordering};

pub fn is_main_thread() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum WaitResult {
    Ok,
    TimedOut,
    Unsupported,
}

pub fn park_wait_at_addr(_ptr: u32) -> WaitResult {
    WaitResult::Unsupported
}

pub fn park_wait_timeout_at_addr(_ptr: u32, _timeout_ms: f64) -> WaitResult {
    WaitResult::Unsupported
}

pub fn park_notify_at_addr(ptr: u32) {
    let token = unsafe { &*(ptr as *const AtomicI32) };
    token.store(1, Ordering::Release);
}

pub fn atomics_wait_timeout_ms_try(_timeout_ms: f64) -> WaitResult {
    WaitResult::Unsupported
}

pub fn sleep_sync_ms(ms: f64) {
    if ms <= 0.0 {
        return;
    }
    let end = wasm_lite::performance::now() + ms;
    while wasm_lite::performance::now() < end {}
}

pub fn get_available_parallelism() -> u32 {
    1
}

#[allow(unused)]
pub fn log_str(s: &str) {
    wasm_lite::console::log(s);
}
