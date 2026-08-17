// SPDX-License-Identifier: MIT OR Apache-2.0
//! Regression fixture: the test macros must reach the runtime through the paths
//! they are *given*, never through absolute `::wasm_lite` / `::wasm_lite_std`.
//!
//! This is what a wrapper crate needs in order to offer wasm_lite's test surface
//! without making every downstream user add both runtimes to their own
//! `Cargo.toml`. Both dependencies are renamed in `Cargo.toml`, so the crate
//! names the expansions used to hard-code are not in this crate's extern prelude
//! at all. If an expansion reintroduces one, this stops compiling with E0433 —
//! which is exactly what a consumer of such a wrapper used to hit.
//!
//! Compile-only by design: the bug is name resolution, and the `(worker)` form
//! needs a shared-memory `+atomics` build to actually run. `scripts/wasm32/check`
//! is the stage that covers it.

// Control: the main-thread form has always honoured `crate =`, so it is here to
// show a failure is specific to the worker arm rather than to `crate =` at large.
#[wl::wasm_lite_test(crate = ::wl)]
fn main_thread_form() {}

// The worker form is the one that hard-coded `::wasm_lite_std`, in all four of
// its paths (`__rt::test_pending`, `spawn_local`, `spawn`, `__rt::test_pass`)
// plus the non-wasm32 fallback arm.
#[wl::wasm_lite_test(worker, crate = ::wl, std_crate = ::wls)]
fn worker_form() {}

// A sync benchmark names only the runtime, so `crate =` alone is enough...
#[wl::wasm_lite_bench(crate = ::wl)]
fn sync_bench(b: &mut wl::Bencher) {
    b.iter(|| ());
}

// ...while an `async fn` benchmark is the other expansion that reaches for the
// std veneer, and had the same hard-coded paths as the worker test form.
#[wl::wasm_lite_bench(crate = ::wl, std_crate = ::wls)]
async fn async_bench(b: &mut wl::Bencher) {
    b.iter_custom_async(|iters| async move {
        let start = wls::time::Instant::now();
        for _ in 0..iters {
            wls::yield_to_event_loop_async().await;
        }
        start.elapsed()
    })
    .await;
}
