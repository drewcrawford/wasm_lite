//! Bindings to the JavaScript `console` global.

// Host functions supplied by the JS loader under the `wasm_lite` import module.
#[link(wasm_import_module = "wasm_lite")]
unsafe extern "C" {
    // Log `len` UTF-8 bytes starting at `ptr` via `console.log`. The host reads
    // (never retains) the bytes straight out of our linear memory.
    fn console_log(ptr: *const u8, len: usize);
}

/// Log a string via the JavaScript `console.log`.
pub fn log(s: &str) {
    // SAFETY: `s` is a valid UTF-8 slice in our linear memory, so `ptr` is
    // valid for `len` bytes for the duration of the call.
    unsafe { console_log(s.as_ptr(), s.len()) }
}
