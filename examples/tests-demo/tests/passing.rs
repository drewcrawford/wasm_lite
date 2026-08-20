// SPDX-License-Identifier: MIT OR Apache-2.0
//! An all-passing suite, to verify a clean run exits 0.

use wasm_lite::wasm_lite_test;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "logwise_v1")]
unsafe extern "C" {
    fn emit(ptr: *const u8, len: usize) -> i32;
}

#[wasm_lite_test]
fn one_plus_one() {
    assert_eq!(1 + 1, 2);
}

#[wasm_lite_test]
fn strings_match() {
    assert_eq!("hi", "hi");
}

/// Proves the reserved structured-observability ABI is installed by the real
/// browser runner, not merely present in generated source snapshots.
#[wasm_lite_test]
fn logwise_host_import_accepts_a_v1_envelope() {
    let event_name = b"wasm_lite.host.test";
    // The fixed header through the event-name prefix is 67 bytes. The zeroed
    // tail encodes empty package/target/module strings, absent domain/test and
    // location, no omitted/retained fields, and no message.
    let mut envelope = vec![0_u8; 83 + event_name.len()];
    let envelope_len = envelope.len() as u32;
    envelope[..6].copy_from_slice(b"LW1\0\x01\0");
    envelope[8..12].copy_from_slice(&envelope_len.to_le_bytes());
    envelope[12..20].copy_from_slice(&1_u64.to_le_bytes());
    // No links. The following bytes are severity, class, and kind.
    envelope[62..65].copy_from_slice(&[2, 0, 0]);
    envelope[65..67].copy_from_slice(&(event_name.len() as u16).to_le_bytes());
    envelope[67..67 + event_name.len()].copy_from_slice(event_name);

    #[cfg(target_arch = "wasm32")]
    assert_eq!(unsafe { emit(envelope.as_ptr(), envelope.len()) }, 0);

    #[cfg(not(target_arch = "wasm32"))]
    assert_eq!(&envelope[..4], b"LW1\0");
}

// A `#[should_panic]` test traps the module. Each test gets a fresh page, so
// the poisoned instance is discarded either way; the runner is what turns the
// trap into a pass.
#[wasm_lite_test]
#[should_panic]
fn panicking_passes_when_expected() {
    panic!("this is supposed to happen");
}

// The expected message is matched against what the panic hook logged.
#[wasm_lite_test]
#[should_panic(expected = "supposed to happen")]
fn panic_message_is_matched() {
    panic!("this is supposed to happen");
}

// Skipped unless the runner is asked for ignored cases; it would fail if run,
// which is what makes the skip observable rather than merely claimed.
#[wasm_lite_test]
#[ignore = "asserts something false, to prove it is not run"]
fn ignored_is_not_run() {
    panic!("an #[ignore]d test was executed");
}

wasm_lite::test_main!();
