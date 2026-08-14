// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser test suite for `wasm_lite_std`, run via the wasm_lite runner.
//!
//! libtest doesn't run on `wasm32-unknown-unknown`, so this is a `harness = false`
//! target using `#[wasm_lite_test]` — the runner discovers each test from the
//! `__wasm_lite_tests` section and drives it in a fresh browser page. Async tests
//! use `async_doctest!` (fail-closed). Run with:
//!
//! ```text
//! RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
//!   -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 \
//!   -C link-arg=--import-memory -C link-arg=--export=__stack_pointer \
//! -C link-arg=--export=__tls_size \
//! -C link-arg=--export=__wasm_init_tls" \
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=$PWD/target/debug/runner \
//! cargo +nightly test -p wasm_lite_std --test browser \
//!   --target wasm32-unknown-unknown -Z build-std=std,panic_abort
//! ```

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

// Host: nothing to run here (the suite targets the browser); a trivial main
// satisfies `harness = false` so the workspace build/test is unaffected.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
#[path = "browser"]
mod suite {
    mod promises;
    mod runtime;
    mod sync;

    use std::sync::Arc;
    use wasm_lite_std::Mutex;
    use wasm_lite_std::condvar::Condvar;
    use wasm_lite_std::time::{Duration, Instant};

    /// Busy-wait for `dur` (used to hold a lock for a bounded time without
    /// `Atomics.wait`, so it works on any thread).
    fn spin_for(dur: Duration) {
        let start = Instant::now();
        while start.elapsed() < dur {
            std::hint::spin_loop();
        }
    }

    /// Which blocking wait strategy a condvar waiter uses.
    #[derive(Clone, Copy)]
    enum CondvarWait {
        Spin,
        Block,
        Sync,
    }

    /// A notifier worker sets a flag + notifies; a waiter worker blocks on the
    /// condvar (via `kind`) until the flag is set. Both are joined. Checking the
    /// predicate under the lock makes the handshake immune to notify/wait ordering.
    fn run_condvar_wait(kind: CondvarWait) {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let notifier = Arc::clone(&pair);
        let n = wasm_lite_std::spawn(move || {
            spin_for(Duration::from_millis(10));
            let (m, cv) = &*notifier;
            *m.lock_sync() = true;
            cv.notify_one();
        });
        let waiter = Arc::clone(&pair);
        let w = wasm_lite_std::spawn(move || {
            let (m, cv) = &*waiter;
            let mut ready = m.lock_sync();
            while !*ready {
                ready = match kind {
                    CondvarWait::Spin => cv.wait_spin(ready),
                    CondvarWait::Block => cv.wait_block(ready),
                    CondvarWait::Sync => cv.wait_sync(ready),
                };
            }
            assert!(*ready);
        });
        n.join().unwrap();
        w.join().unwrap();
    }
}
