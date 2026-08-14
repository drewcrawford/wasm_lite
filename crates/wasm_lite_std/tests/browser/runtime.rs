// SPDX-License-Identifier: MIT OR Apache-2.0
//! Threads, timers, and time: spawning, `sleep_async`, animation frames,
//! `Instant`/`SystemTime`, and the executor's cross-thread wakeups.

use super::spin_for;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::task::{Context, Poll};
use wasm_lite_std::condvar::Condvar;
use wasm_lite_std::rwlock::RwLock;
use wasm_lite_std::time::{Duration, Instant};
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

/// `sleep_async` waits without blocking, and for at least as long as asked.
///
/// On the main thread, which is the case that matters: `sleep` would trap
/// there (`Atomics.wait` is forbidden), so an executor scheduling work for
/// later has nothing else to use.
#[wasm_lite::wasm_lite_test]
fn sleep_async_waits_on_the_main_thread() {
    wasm_lite_std::async_doctest!(async {
        let start = Instant::now();
        wasm_lite_std::sleep_async(Duration::from_millis(30)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(25),
            "slept {elapsed:?}, expected at least ~30ms"
        );
    });
}

/// A `Send` sleep may be created on one worker and awaited in another realm.
/// Its timer must survive the creating worker exiting.
#[wasm_lite::wasm_lite_test]
fn regression_sleep_async_survives_its_creator_worker_exiting() {
    struct FirstReady<A, B> {
        preferred: A,
        watchdog: B,
    }

    impl<A: Future<Output = ()> + Unpin, B: Future<Output = ()> + Unpin> Future for FirstReady<A, B> {
        type Output = bool;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
            if Pin::new(&mut self.preferred).poll(cx).is_ready() {
                return Poll::Ready(true);
            }
            if Pin::new(&mut self.watchdog).poll(cx).is_ready() {
                return Poll::Ready(false);
            }
            Poll::Pending
        }
    }

    wasm_lite_std::async_doctest!(async {
        let moved = wasm_lite_std::spawn(|| wasm_lite_std::sleep_async(Duration::from_millis(20)))
            .join_async()
            .await
            .unwrap();
        let watchdog = wasm_lite_std::sleep_async(Duration::from_millis(300));

        assert!(
            FirstReady {
                preferred: moved,
                watchdog,
            }
            .await,
            "SleepAsync was orphaned when its creating worker exited"
        );
    });
}

/// `request_animation_frame` runs its callback, and releases it after.
///
/// The closure has to outlive the call — it is what JS will invoke — so it
/// is parked in a thread-local rather than forgotten. This checks the
/// callback actually fires, and that scheduling the next frame from inside
/// it works, which is the case where releasing the slot too eagerly would
/// free the closure that just replaced it.
#[wasm_lite::wasm_lite_test]
fn request_animation_frame_fires_and_chains() {
    use std::rc::Rc;
    wasm_lite_std::async_doctest!(async {
        let hits = Rc::new(std::cell::Cell::new(0));
        let (tx, mut rx) = mpsc::channel();

        let h1 = hits.clone();
        let tx1 = tx.clone();
        wasm_lite_std::request_animation_frame(move || {
            h1.set(h1.get() + 1);
            // Schedule the next frame from inside this one.
            let h2 = h1.clone();
            wasm_lite_std::request_animation_frame(move || {
                h2.set(h2.get() + 1);
                tx1.send_spin(()).unwrap();
            });
        });

        rx.recv_async().await.unwrap();
        assert_eq!(hits.get(), 2, "both frames ran");
    });
}

/// The sleep future must be `Send`.
///
/// It holds a realm-bound `Closure` in spirit, and an executor that cannot
/// move the task has lost most of the point — portable_async_sleep asserts
/// exactly this about its own future.
#[wasm_lite::wasm_lite_test]
fn the_sleep_future_is_send() {
    fn assert_send<T: Send>(_: T) {}
    assert_send(wasm_lite_std::sleep_async(Duration::from_millis(1)));
}

/// A sleep longer than `setTimeout` can express must not fire immediately.
///
/// Browsers truncate the delay to `i32`, so a naive 30-day `setTimeout`
/// fires at once. `sleep_async` chains legs instead. Polling it once and
/// finding it pending is the whole assertion — actually waiting is not an
/// option.
#[wasm_lite::wasm_lite_test]
fn a_very_long_sleep_does_not_fire_at_once() {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    let mut long = Box::pin(wasm_lite_std::sleep_async(Duration::from_secs(
        60 * 60 * 24 * 30,
    )));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        matches!(Pin::new(&mut long).poll(&mut cx), Poll::Pending),
        "a 30-day sleep resolved immediately: the delay was truncated"
    );
}

/// Dropping a pending sleep cancels its timer rather than leaving a callback
/// to fire into freed state.
#[wasm_lite::wasm_lite_test]
fn dropping_a_pending_sleep_is_safe() {
    wasm_lite_std::async_doctest!(async {
        let sleep = wasm_lite_std::sleep_async(Duration::from_secs(30));
        drop(sleep);
        // Still alive and still scheduling afterwards.
        wasm_lite_std::sleep_async(Duration::from_millis(5)).await;
    });
}

/// Two threads must agree on `Instant`.
///
/// `performance.now()` is **per-realm**: a worker's zero is the moment that
/// worker started, so a reading taken there is smaller than one taken on the
/// main thread at the same moment — by however long the page had been up.
/// `Instant` is `Ord` and deadlines cross threads constantly (`join_async`,
/// `lock_block_timeout`, an executor's `poll_after`), so the time origin has
/// to be folded in. Without that this asserts backwards.
///
/// `(worker)` because `join` blocks, which the browser main thread may not.
/// Two realms is the point; which two does not matter.
#[wasm_lite::wasm_lite_test(worker)]
fn instants_are_comparable_across_threads() {
    // Give the page a measurable age first, so a worker-relative clock is
    // visibly wrong rather than merely imprecise.
    spin_for(Duration::from_millis(50));
    let before = Instant::now();
    let (on_worker, after) = wasm_lite_std::spawn(move || {
        let t = Instant::now();
        spin_for(Duration::from_millis(5));
        (t, Instant::now())
    })
    .join()
    .unwrap();

    assert!(
        on_worker >= before,
        "a worker's Instant is on a different timeline: {on_worker:?} < {before:?}"
    );
    assert!(after > on_worker, "and time still moves forward on it");
    // Sanity bound: the same timeline, not merely a bigger number.
    assert!(
        after.duration_since(before) < Duration::from_secs(30),
        "the two threads' clocks are wildly apart"
    );
}

/// A worker spawning a worker, then blocking on it.
///
/// Chrome fetches a nested worker's module script *through its parent*, and
/// a parent sitting in `Atomics.wait` never services that fetch — so the
/// child never starts and the `join` below never returns. Every blocking
/// primitive we have sits in `Atomics.wait`, which made this the default
/// way to deadlock. Firefox does not do it, which is why a Firefox-only run
/// of this suite was green while Chrome hung here.
///
/// Worth its own test rather than leaving it to `mutex_lock_block`: that
/// one happens to nest, this one is *about* nesting, and the difference
/// matters when someone reads the failure.
#[wasm_lite::wasm_lite_test(worker)]
fn a_worker_can_spawn_a_worker() {
    let v = wasm_lite_std::spawn(|| 20 + 22).join().unwrap();
    assert_eq!(v, 42);
}

/// Nesting composes: the grandchild is reached through two parents, both of
/// which are blocked by the time it starts.
#[wasm_lite::wasm_lite_test(worker)]
fn nesting_composes_to_a_third_level() {
    let v = wasm_lite_std::spawn(|| wasm_lite_std::spawn(|| 7u32).join().unwrap() * 6)
        .join()
        .unwrap();
    assert_eq!(v, 42);
}

/// `mpsc` cross-thread: a worker sends, the main thread `recv_async`s.
#[wasm_lite::wasm_lite_test]
fn mpsc_cross_thread() {
    wasm_lite_std::async_doctest!(async {
        let (tx, mut rx) = mpsc::channel();
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

/// `Instant` (performance.now-backed) is monotonic and supports arithmetic.
#[wasm_lite::wasm_lite_test]
fn time_instant_monotonic() {
    use wasm_lite_std::time::Instant;
    let a = Instant::now();
    let b = Instant::now();
    assert!(b >= a);
    assert_eq!(a.saturating_duration_since(b), Duration::ZERO);

    let later = a + Duration::from_secs(5);
    assert_eq!(later - a, Duration::from_secs(5));
    assert!(later > a);
}

/// `SystemTime` (Date.now-backed) sits after the Unix epoch.
#[wasm_lite::wasm_lite_test]
fn time_systemtime_after_epoch() {
    use wasm_lite_std::time::{SystemTime, UNIX_EPOCH};
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("now() is after the Unix epoch");
    assert!(since_epoch > Duration::from_secs(1_000_000_000));

    let err = (UNIX_EPOCH + Duration::from_secs(10))
        .duration_since(UNIX_EPOCH + Duration::from_secs(40))
        .unwrap_err();
    assert_eq!(err.duration(), Duration::from_secs(30));
}

/// `Instant::elapsed()` advances after sleeping on a worker thread.
#[wasm_lite::wasm_lite_test]
fn time_instant_elapsed_after_sleep() {
    wasm_lite_std::async_doctest!(async {
        use wasm_lite_std::time::Instant;
        let elapsed = wasm_lite_std::spawn(|| {
            let start = Instant::now();
            wasm_lite_std::sleep(Duration::from_millis(20));
            start.elapsed()
        })
        .join_async()
        .await
        .unwrap();
        assert!(
            elapsed >= Duration::from_millis(20),
            "elapsed {elapsed:?} should be >= 20ms"
        );
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

/// On its first poll this future calls `spawn()` to schedule its own wakeup,
/// returns `Pending`, and expects a later poll (driven by `waker.wake()` from
/// the worker) to complete it. Ported from the old wasm-backend suite — it
/// reproduces a Chrome stall where spawning from inside `poll` failed to wake
/// the task.
struct WakeFromSpawnFuture {
    scheduled: bool,
    awoken: Arc<AtomicBool>,
}

impl Future for WakeFromSpawnFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.awoken.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        if !self.scheduled {
            self.scheduled = true;
            let waker = cx.waker().clone();
            let awoken = Arc::clone(&self.awoken);
            wasm_lite_std::spawn(move || {
                wasm_lite_std::sleep(Duration::from_millis(5));
                awoken.store(true, Ordering::SeqCst);
                waker.wake();
            });
        }
        Poll::Pending
    }
}

/// The test harness drives each test on the browser main thread, so
/// `is_main_thread()` must report `true` there; a spawned worker reports `false`.
#[wasm_lite::wasm_lite_test]
fn is_main_thread_reports_main_and_worker() {
    assert!(
        wasm_lite_std::is_main_thread(),
        "the test body runs on the browser main thread"
    );
    wasm_lite_std::async_doctest!(async {
        let from_worker = wasm_lite_std::spawn(wasm_lite_std::is_main_thread)
            .join_async()
            .await
            .unwrap();
        assert!(!from_worker, "a spawned Web Worker is not the main thread");
    });
}

/// `spawn()` from inside `Future::poll` must wake the pending future.
#[wasm_lite::wasm_lite_test]
fn spawn_from_poll_pending_wakes_future() {
    wasm_lite_std::async_doctest!(async {
        let awoken = Arc::new(AtomicBool::new(false));
        let fut = WakeFromSpawnFuture {
            scheduled: false,
            awoken: Arc::clone(&awoken),
        };
        fut.await;
        assert!(
            awoken.load(Ordering::SeqCst),
            "future should be awoken by spawned worker"
        );
    });
}
