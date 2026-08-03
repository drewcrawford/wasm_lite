// SPDX-License-Identifier: MIT OR Apache-2.0
//! Benchmarking: `#[wasm_lite_bench]` and [`Bencher`].
//!
//! The counterpart of `#[wasm_lite_test]`. A benchmark takes a `&mut Bencher`
//! and hands it the work to measure; the harness decides how many times to run
//! it and reports nanoseconds per iteration, in the shape `cargo bench` prints:
//!
//! ```text
//! test parse::small_document ... bench:         412 ns/iter (+/- 27)
//! ```
//!
//! ```
//! use wasm_lite::Bencher;
//!
//! #[wasm_lite::wasm_lite_bench]
//! fn sum_to_1000(b: &mut Bencher) {
//!     b.iter(|| (0u64..1000).sum::<u64>());
//! }
//! # fn main() {}
//! ```
//!
//! # Why not criterion
//!
//! Criterion needs a wall clock it can trust at sub-microsecond resolution,
//! spawns processes for its baseline comparisons, and writes its history to
//! disk. A browser tab has none of those. What it does have is
//! `performance.now()`, which even under cross-origin isolation is coarsened to
//! **5 µs** in Chrome (and to 1 ms without it) as a Spectre mitigation.
//!
//! That single fact sets the design: a timer that cannot resolve one iteration
//! must time a *batch* of them. So the harness calibrates a batch size before
//! measuring anything, and every number it reports is a batch average. The
//! consequence is worth stating plainly — this measures throughput, and it
//! cannot see per-iteration variance below the batch. It is the right tool for
//! "did this change make it faster", and the wrong one for tail latency.
//!
//! # What the numbers mean
//!
//! `iter` runs the body in batches of `n`, where `n` is chosen so one batch
//! takes at least [`CALIBRATION_MS`] — long enough that the clock's granularity
//! contributes under 0.1% of the reading. It then takes [`SAMPLES`] such
//! batches and reports the **median** ns/iter, with `+/-` being the full
//! spread (max − min) across samples, exactly as libtest's `cargo bench` does.
//!
//! Median rather than mean because a browser will occasionally interrupt a
//! sample with GC or another tab's work, and one such sample would drag a mean
//! far more than it drags the truth. The spread is deliberately the *full*
//! range rather than a standard deviation: it makes those interruptions
//! visible instead of averaging them away, so a suspiciously wide `+/-` is a
//! signal that the machine was busy and the run should be repeated.

use std::cell::Cell;
use std::hint::black_box;

/// Minimum duration of one timed batch, in milliseconds.
///
/// `performance.now()` is coarsened to 5 µs in a cross-origin-isolated page, so
/// a 10 ms batch quantizes to at worst 0.05% — below the noise floor of
/// anything a browser can measure. Raising it buys precision the platform
/// cannot deliver; lowering it lets the clock's granularity into the reading.
pub const CALIBRATION_MS: f64 = 10.0;

/// Number of timed batches taken per benchmark.
///
/// With [`CALIBRATION_MS`] this is ~0.5 s of measurement per benchmark, plus
/// calibration. Enough samples for a median to mean something, few enough that
/// a suite of them still finishes inside the runner's per-page timeout.
pub const SAMPLES: usize = 50;

/// Ceiling on the batch size, so a body that is optimized away entirely (or is
/// simply free) terminates calibration instead of doubling forever.
const MAX_ITERS: u64 = 1 << 40;

/// Handed to a `#[wasm_lite_bench]` function; the benchmark gives it the work
/// to measure via [`iter`](Bencher::iter).
///
/// A benchmark that never calls `iter` reports nothing and the runner fails it,
/// rather than reporting a fabricated zero.
pub struct Bencher {
    result: Option<Report>,
}

/// What one benchmark measured.
#[derive(Clone, Copy, Debug)]
struct Report {
    /// Iterations per timed batch, as chosen by calibration.
    iters: u64,
    median_ns: f64,
    min_ns: f64,
    max_ns: f64,
}

impl Default for Bencher {
    fn default() -> Self {
        Self::new()
    }
}

impl Bencher {
    /// Create an unmeasured bencher. The `#[wasm_lite_bench]` entry point does
    /// this for you; call it directly only when driving a benchmark by hand.
    pub fn new() -> Self {
        Bencher { result: None }
    }

    /// Measure `f`, calibrating a batch size first.
    ///
    /// The body's return value is passed through [`black_box`] so the optimizer
    /// cannot delete the work for being unused. That is a fence, not a
    /// guarantee: a body whose result does not depend on anything opaque can
    /// still be hoisted out of the loop, and the tell is a result at or near
    /// zero. Take an input through `black_box` if you see one.
    ///
    /// Calling `iter` more than once keeps the last measurement.
    pub fn iter<T, F>(&mut self, mut f: F)
    where
        F: FnMut() -> T,
    {
        let iters = calibrate(&mut f);

        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let ms = time_batch(iters, &mut f);
            // ms → ns, divided across the batch.
            samples.push(ms * 1.0e6 / iters as f64);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).expect("timings are never NaN"));

        self.result = Some(Report {
            iters,
            median_ns: samples[samples.len() / 2],
            min_ns: samples[0],
            max_ns: samples[samples.len() - 1],
        });
    }

    /// Publish the measurement where the runner can read it.
    ///
    /// Called by the generated `#[wasm_lite_bench]` entry point after the
    /// benchmark body returns. Not part of the stable surface.
    #[doc(hidden)]
    pub fn __wl_record(&self) {
        // These readers are reached only from the runner's JS, so nothing in
        // the Rust call graph keeps them and wasm-ld would garbage-collect
        // them. wasm32-only, matching `Closure`'s trampolines: forcing them out
        // on the host is unnecessary there (nothing GCs them) and would only
        // widen what a native link has to resolve.
        #[cfg(target_arch = "wasm32")]
        #[used]
        static KEEP: [extern "C" fn() -> f64; 4] = [
            __wl_bench_median_ns,
            __wl_bench_min_ns,
            __wl_bench_max_ns,
            __wl_bench_iters,
        ];

        let r = self.result.unwrap_or(Report {
            iters: 0,
            median_ns: 0.0,
            min_ns: 0.0,
            max_ns: 0.0,
        });
        LAST.with(|c| c.set(r));
    }
}

thread_local! {
    /// The most recent measurement, read back through the exports below.
    ///
    /// One slot rather than a map: the runner loads a fresh page per benchmark
    /// (as it does per test), so a module only ever holds one result.
    static LAST: Cell<Report> = const {
        Cell::new(Report { iters: 0, median_ns: 0.0, min_ns: 0.0, max_ns: 0.0 })
    };
}

/// Nanoseconds per iteration, median across samples. `0` means the benchmark
/// never called [`Bencher::iter`].
#[unsafe(no_mangle)]
pub extern "C" fn __wl_bench_median_ns() -> f64 {
    LAST.with(|c| c.get().median_ns)
}

/// Fastest sample, ns/iter.
#[unsafe(no_mangle)]
pub extern "C" fn __wl_bench_min_ns() -> f64 {
    LAST.with(|c| c.get().min_ns)
}

/// Slowest sample, ns/iter.
#[unsafe(no_mangle)]
pub extern "C" fn __wl_bench_max_ns() -> f64 {
    LAST.with(|c| c.get().max_ns)
}

/// Iterations per timed batch. `0` means nothing was measured.
///
/// `f64` rather than `u64` so the value reaches JS as a Number: the batch size
/// is well inside 2^53, and a `u64` export would arrive as a `BigInt` the
/// runner would then have to convert back.
#[unsafe(no_mangle)]
pub extern "C" fn __wl_bench_iters() -> f64 {
    LAST.with(|c| c.get().iters as f64)
}

/// Choose a batch size whose timed duration clears [`CALIBRATION_MS`].
///
/// Doubling from 1 would need ~20 rounds for a nanosecond-scale body, each
/// paying a page-visible timer read. Instead each round extrapolates: if a
/// batch took a tenth of the target, ask for ten times as many. The growth is
/// clamped because the first reading of a JIT-cold body is the least
/// trustworthy one — an unclamped extrapolation from it can overshoot by
/// orders of magnitude and spend minutes in a single benchmark.
fn calibrate<T, F>(f: &mut F) -> u64
where
    F: FnMut() -> T,
{
    let mut n: u64 = 1;
    loop {
        let ms = time_batch(n, f);
        if ms >= CALIBRATION_MS || n >= MAX_ITERS {
            return n;
        }
        // A zero reading means the batch finished inside one clock tick, which
        // says nothing about how much bigger it should be — so take a fixed
        // step rather than dividing by zero.
        let grow = if ms > 0.0 {
            (CALIBRATION_MS / ms).ceil() as u64
        } else {
            16
        };
        n = n.saturating_mul(grow.clamp(2, 64)).min(MAX_ITERS);
    }
}

/// Run `n` iterations and return the elapsed milliseconds.
fn time_batch<T, F>(n: u64, f: &mut F) -> f64
where
    F: FnMut() -> T,
{
    let start = now_ms();
    for _ in 0..n {
        black_box(f());
    }
    now_ms() - start
}

/// Milliseconds from an arbitrary epoch.
///
/// `performance.now()` in a browser; `Instant` on the host, so a benchmark can
/// be run natively (`cargo test`, a doctest) without a browser in the loop.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    crate::performance::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    use std::time::Instant;
    thread_local! {
        static EPOCH: Instant = Instant::now();
    }
    EPOCH.with(|e| e.elapsed().as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calibration has to terminate on a body too cheap to time, rather than
    /// doubling until it overflows.
    #[test]
    fn calibration_terminates_on_a_free_body() {
        let mut b = Bencher::new();
        b.iter(|| 1u32);
        let r = b.result.expect("iter records a result");
        assert!(r.iters > 0, "a batch size was chosen");
        assert!(r.median_ns >= 0.0);
    }

    /// The reported figure is per *iteration*, not per batch — the division by
    /// the batch size is the one place an off-by-a-factor-of-`n` could hide.
    ///
    /// Uses a body with a floor on its cost so the assertion is about
    /// arithmetic, not about the machine.
    #[test]
    fn the_result_is_per_iteration() {
        let mut one = Bencher::new();
        one.iter(|| black_box(1u64).wrapping_mul(3));

        let mut ten = Bencher::new();
        ten.iter(|| {
            let mut acc = black_box(1u64);
            for _ in 0..10 {
                acc = acc.wrapping_mul(3);
            }
            black_box(acc)
        });

        let a = one.result.expect("measured").median_ns;
        let b = ten.result.expect("measured").median_ns;
        // Ten times the work must not be *cheaper* per iteration. A loose
        // bound: this is a real machine and the point is the scaling, not a
        // specific ratio.
        assert!(
            b >= a,
            "ten multiplies ({b} ns) should not beat one ({a} ns)"
        );
    }

    /// A benchmark that never measured anything must report zero, so the runner
    /// can tell it apart from one that measured an immeasurably fast body.
    #[test]
    fn an_unmeasured_bench_reports_zero_iterations() {
        let b = Bencher::new();
        b.__wl_record();
        assert_eq!(__wl_bench_iters(), 0.0);
        assert_eq!(__wl_bench_median_ns(), 0.0);
    }

    /// min <= median <= max, or the runner's `+/-` is nonsense.
    #[test]
    fn the_samples_are_ordered() {
        let mut b = Bencher::new();
        b.iter(|| black_box(7u64).wrapping_add(1));
        let r = b.result.expect("measured");
        assert!(r.min_ns <= r.median_ns, "{} <= {}", r.min_ns, r.median_ns);
        assert!(r.median_ns <= r.max_ns, "{} <= {}", r.median_ns, r.max_ns);
    }
}
