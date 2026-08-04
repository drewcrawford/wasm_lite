// SPDX-License-Identifier: MIT OR Apache-2.0
//! Benchmarks run in a real browser, driven by the wasm_lite runner.
//!
//! `cargo bench` prints the measurements; `cargo test --benches` runs each one
//! once without measuring, which is what keeps them working in CI.

use std::hint::black_box;
use wasm_lite::{Bencher, wasm_lite_bench};

/// The floor: an empty body. Whatever this reports is the harness's own
/// per-iteration overhead, so every other number here should be read against
/// it rather than against zero.
#[wasm_lite_bench]
fn empty(b: &mut Bencher) {
    b.iter(|| ());
}

/// Work the optimizer cannot hoist, because both the input and the output pass
/// through `black_box`.
#[wasm_lite_bench]
fn multiply(b: &mut Bencher) {
    b.iter(|| black_box(black_box(6364136223846793005u64).wrapping_mul(1442695040888963407)));
}

/// Enough arithmetic to sit clearly above the noise floor, so a regression in
/// the harness's own accounting would be visible as a changed ratio against
/// `multiply`.
#[wasm_lite_bench]
fn sum_to_1000(b: &mut Bencher) {
    b.iter(|| black_box((0u64..black_box(1000)).sum::<u64>()));
}

/// Allocation, which is the cheapest way to show a benchmark reaching outside
/// pure arithmetic into the wasm heap.
#[wasm_lite_bench]
fn allocate_a_vec(b: &mut Bencher) {
    b.iter(|| {
        let v: Vec<u64> = (0..black_box(64)).collect();
        black_box(v)
    });
}

/// An async benchmark that times itself.
///
/// `iter_custom_async` exists for work that cannot be driven synchronously —
/// a `requestAnimationFrame` loop, an awaited GPU submission — and for work
/// whose setup should be excluded from the measurement. The routine gets the
/// iteration count and reports the duration it measured.
#[wasm_lite_bench]
async fn awaited_yield(b: &mut Bencher) {
    b.iter_custom_async(|iters| async move {
        let start = wasm_lite_std::time::Instant::now();
        for _ in 0..iters {
            wasm_lite_std::yield_to_event_loop_async().await;
        }
        start.elapsed()
    })
    .await;
}

wasm_lite::bench_main!();
