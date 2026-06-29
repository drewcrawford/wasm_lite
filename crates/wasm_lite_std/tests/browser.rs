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
//!   -C link-arg=--export=__tls_base -C link-arg=--export=__tls_size \
//!   -C link-arg=--export=__tls_align -C link-arg=--export=__wasm_init_tls" \
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
mod suite {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use wasm_lite_std::condvar::Condvar;
    use wasm_lite_std::rwlock::RwLock;
    use wasm_lite_std::{Mutex, mpsc};

    /// Uncontended `Mutex` on the main thread (sync, no waiting).
    #[wasm_lite::wasm_lite_test]
    fn mutex_uncontended() {
        let m = Mutex::new(40);
        *m.lock_sync() += 2;
        assert_eq!(*m.lock_sync(), 42);
    }

    /// `spawn` + `join_async` across threads.
    #[wasm_lite::wasm_lite_test]
    fn spawn_join_async() {
        wasm_lite_std::async_doctest!(async {
            let v = wasm_lite_std::spawn(|| 20 + 22).join_async().await.unwrap();
            assert_eq!(v, 42);
        });
    }

    /// `mpsc` cross-thread: a worker sends, the main thread `recv_async`s.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_cross_thread() {
        wasm_lite_std::async_doctest!(async {
            let (tx, rx) = mpsc::channel();
            wasm_lite_std::spawn(move || {
                tx.send_sync(7).unwrap();
            });
            assert_eq!(rx.recv_async().await.unwrap(), 7);
        });
    }

    static M: Mutex<u32> = Mutex::new(0);
    static HOLDING: AtomicU32 = AtomicU32::new(0);

    /// Contended `Mutex`: a worker holds it, mutates, releases; the main thread
    /// `lock_async`s and is woken cross-thread.
    #[wasm_lite::wasm_lite_test]
    fn mutex_async_contended() {
        wasm_lite_std::async_doctest!(async {
            wasm_lite_std::spawn(|| {
                let mut g = M.lock_sync();
                *g = 100;
                HOLDING.store(1, Ordering::SeqCst);
                wasm_lite_std::sleep(Duration::from_millis(30));
                *g += 23;
            });
            while HOLDING.load(Ordering::SeqCst) == 0 {
                wasm_lite_std::yield_to_event_loop_async().await;
            }
            let g = M.lock_async().await;
            assert_eq!(*g, 123);
        });
    }

    /// `RwLock` write then read (async).
    #[wasm_lite::wasm_lite_test]
    fn rwlock_write_then_read() {
        wasm_lite_std::async_doctest!(async {
            let lock = RwLock::new(10);
            *lock.lock_async_write().await += 5;
            assert_eq!(*lock.lock_async_read().await, 15);
        });
    }

    static CV_M: Mutex<u32> = Mutex::new(0);
    static CV: Condvar = Condvar::new();

    /// `Condvar`: a worker sets a value under the lock and notifies; the main
    /// thread waits on the condvar (released across the await) until the
    /// predicate holds.
    #[wasm_lite::wasm_lite_test]
    fn condvar_cross_thread() {
        wasm_lite_std::async_doctest!(async {
            wasm_lite_std::spawn(|| {
                let mut g = CV_M.lock_sync();
                *g = 7;
                CV.notify_one();
            });
            let mut g = CV_M.lock_async().await;
            while *g == 0 {
                g = CV.wait_async(g).await;
            }
            assert_eq!(*g, 7);
        });
    }
}
