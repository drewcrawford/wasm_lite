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
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::task::{Context, Poll};
    use wasm_lite_std::condvar::Condvar;
    use wasm_lite_std::rwlock::RwLock;
    use wasm_lite_std::spinlock::Spinlock;
    use wasm_lite_std::time::{Duration, Instant};
    use wasm_lite_std::{Mutex, mpsc};

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

    // =====================================================================
    // Ported unit tests. Naming: tests whose body must block (`*_block`,
    // `recv_block`, blocking `into_iter`, deadlines elapsing while another
    // thread holds the lock) use `#[wasm_lite_test(worker)]` so `Atomics.wait`
    // is available; non-blocking / async / uncontended ones run on the main
    // thread. Cross-thread coordination uses `join()` / `mpsc`.
    // =====================================================================

    // ---- Spinlock ----

    /// Uncontended `Spinlock::with_mut`.
    #[wasm_lite::wasm_lite_test]
    fn spinlock_basic() {
        let s = Spinlock::new(42);
        let r = s.with_mut(|d| {
            *d += 1;
            *d
        });
        assert_eq!(r, 43);
    }

    /// 10 workers each do 100 `with_mut` increments; total must be 1000.
    #[wasm_lite::wasm_lite_test(worker)]
    fn spinlock_concurrent_access() {
        let s = Arc::new(Spinlock::new(0u32));
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let s = Arc::clone(&s);
                wasm_lite_std::spawn(move || {
                    for _ in 0..100 {
                        s.with_mut(|d| *d += 1);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.with_mut(|d| *d), 1000);
    }

    // ---- Mutex ----

    /// `try_lock` on an uncontended mutex succeeds.
    #[wasm_lite::wasm_lite_test]
    fn mutex_try_lock_success() {
        let m = Mutex::new(42);
        assert_eq!(*m.try_lock().unwrap(), 42);
    }

    /// `try_lock` fails for a second holder while the lock is held.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_try_lock_contention() {
        let m = Arc::new(Mutex::new(42));
        let guard = m.try_lock().unwrap();
        let m2 = Arc::clone(&m);
        let failed = wasm_lite_std::spawn(move || m2.try_lock().is_err())
            .join()
            .unwrap();
        assert!(failed);
        drop(guard);
    }

    /// `lock_spin` round-trips a value (uncontended).
    #[wasm_lite::wasm_lite_test]
    fn mutex_lock_spin() {
        let m = Mutex::new(0);
        *m.lock_spin() = 42;
        assert_eq!(*m.lock_spin(), 42);
    }

    /// A worker takes `lock_block`, writes, releases; the body then `lock_block`s
    /// and observes the write.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_lock_block() {
        let m = Arc::new(Mutex::new(0));
        let m2 = Arc::clone(&m);
        wasm_lite_std::spawn(move || {
            *m2.lock_block() = 42;
        })
        .join()
        .unwrap();
        assert_eq!(*m.lock_block(), 42);
    }

    /// 10 workers each take `lock_spin` 100 times and increment; total is 1000.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_concurrent_increment() {
        let m = Arc::new(Mutex::new(0u32));
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let m = Arc::clone(&m);
                wasm_lite_std::spawn(move || {
                    for _ in 0..100 {
                        *m.lock_spin() += 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*m.lock_spin(), 1000);
    }

    /// `lock_async` on an uncontended mutex.
    #[wasm_lite::wasm_lite_test]
    fn mutex_lock_async() {
        wasm_lite_std::async_doctest!(async {
            let m = Mutex::new(42);
            assert_eq!(*m.lock_async().await, 42);
        });
    }

    /// Dropping a guard releases the lock for a later `try_lock`.
    #[wasm_lite::wasm_lite_test]
    fn mutex_guard_drop_releases() {
        let m = Mutex::new(42);
        {
            let _g = m.lock_spin();
        }
        assert_eq!(*m.try_lock().unwrap(), 42);
    }

    /// `lock_spin_timeout` succeeds immediately on a free lock.
    #[wasm_lite::wasm_lite_test]
    fn mutex_lock_spin_timeout_success() {
        let m = Mutex::new(0);
        let deadline = Instant::now() + Duration::from_secs(1);
        *m.lock_spin_timeout(deadline).expect("free lock") = 42;
        assert_eq!(*m.lock_spin(), 42);
    }

    /// `lock_spin_timeout` returns `None` while another thread holds the lock.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_lock_spin_timeout_fails() {
        let m = Arc::new(Mutex::new(0));
        let (tx, rx) = mpsc::channel();
        let m2 = Arc::clone(&m);
        wasm_lite_std::spawn(move || {
            let _g = m2.lock_spin();
            tx.send_block(()).unwrap(); // acquired
            spin_for(Duration::from_millis(100));
        });
        rx.recv_block().unwrap();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(m.lock_spin_timeout(deadline).is_none());
    }

    /// `lock_block_timeout`: `None` while held, then `Some` after release.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_lock_block_timeout() {
        let m = Arc::new(Mutex::new(0));
        let (tx, rx) = mpsc::channel();
        let m2 = Arc::clone(&m);
        wasm_lite_std::spawn(move || {
            let g = m2.lock_block();
            tx.send_block(()).unwrap(); // acquired
            spin_for(Duration::from_millis(50));
            drop(g);
            tx.send_block(()).unwrap(); // released
        });
        rx.recv_block().unwrap();
        assert!(
            m.lock_block_timeout(Instant::now() + Duration::from_millis(10))
                .is_none()
        );
        rx.recv_block().unwrap();
        let g = m
            .lock_block_timeout(Instant::now() + Duration::from_secs(1))
            .expect("acquire after release");
        assert_eq!(*g, 0);
    }

    /// `lock_sync_timeout` returns `None` while another thread holds the lock.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mutex_lock_sync_timeout_fails() {
        let m = Arc::new(Mutex::new(0));
        let (tx, rx) = mpsc::channel();
        let m2 = Arc::clone(&m);
        wasm_lite_std::spawn(move || {
            let _g = m2.lock_sync();
            tx.send_block(()).unwrap();
            spin_for(Duration::from_millis(100));
        });
        rx.recv_block().unwrap();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(m.lock_sync_timeout(deadline).is_none());
    }

    /// `lock_async_timeout`: succeeds on a free lock, times out while held.
    ///
    /// The contended case is a regression test: the deadline must win even when
    /// the holder releases (and thus notifies) well after the deadline has
    /// elapsed — see the `deadline`-authoritative `Race` in `mutex/async_impl.rs`.
    #[wasm_lite::wasm_lite_test]
    fn mutex_lock_async_timeout() {
        wasm_lite_std::async_doctest!(async {
            let m = Arc::new(Mutex::new(0));
            let deadline = Instant::now() + Duration::from_secs(1);
            assert!(m.lock_async_timeout(deadline).await.is_some());

            let (tx, rx) = mpsc::channel();
            let m2 = Arc::clone(&m);
            wasm_lite_std::spawn(move || {
                let _g = m2.lock_block();
                tx.send_block(()).unwrap();
                spin_for(Duration::from_millis(100));
            });
            rx.recv_async().await.unwrap();
            let deadline = Instant::now() + Duration::from_millis(10);
            assert!(m.lock_async_timeout(deadline).await.is_none());
        });
    }

    // ---- mpsc ----

    /// `send_spin` then `recv_spin` with a value already queued.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_send_recv_spin() {
        let (tx, rx) = mpsc::channel();
        tx.send_spin(1).unwrap();
        assert_eq!(rx.recv_spin(), Ok(1));
    }

    /// `send_block` / `recv_block` (would trap on the main thread).
    #[wasm_lite::wasm_lite_test(worker)]
    fn mpsc_send_recv_block() {
        let (tx, rx) = mpsc::channel();
        tx.send_block(1).unwrap();
        assert_eq!(rx.recv_block(), Ok(1));
    }

    /// `send_async` / `recv_async`.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_send_recv_async() {
        wasm_lite_std::async_doctest!(async {
            let (tx, rx) = mpsc::channel();
            tx.send_async(1).await.unwrap();
            assert_eq!(rx.recv_async().await, Ok(1));
        });
    }

    /// Multiple senders preserve per-send delivery; FIFO ordering.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_multiple_senders_and_ordering() {
        let (tx, rx) = mpsc::channel();
        let tx2 = tx.clone();
        tx.send_sync(1).unwrap();
        tx2.send_sync(2).unwrap();
        assert_eq!(rx.recv_sync(), Ok(1));
        assert_eq!(rx.recv_sync(), Ok(2));
    }

    /// A worker sleeps then sends; the body `recv_block`s for it.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mpsc_blocking_behavior() {
        let (tx, rx) = mpsc::channel();
        wasm_lite_std::spawn(move || {
            wasm_lite_std::sleep(Duration::from_millis(10));
            tx.send_block(42).unwrap();
        });
        assert_eq!(rx.recv_block(), Ok(42));
    }

    /// `try_recv` reports `Empty`, then the queued value, then `Empty`.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_try_recv() {
        let (tx, rx) = mpsc::channel();
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        tx.send_sync(1).unwrap();
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    /// `recv_spin_timeout`: value present succeeds; empty times out.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_recv_spin_timeout() {
        let (tx, rx) = mpsc::channel();
        tx.send_spin(1).unwrap();
        assert_eq!(
            rx.recv_spin_timeout(Instant::now() + Duration::from_secs(1)),
            Ok(1)
        );
        assert_eq!(
            rx.recv_spin_timeout(Instant::now() + Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    /// `recv_block_timeout`: value present succeeds; empty times out.
    #[wasm_lite::wasm_lite_test(worker)]
    fn mpsc_recv_block_timeout() {
        let (tx, rx) = mpsc::channel();
        tx.send_block(1).unwrap();
        assert_eq!(
            rx.recv_block_timeout(Instant::now() + Duration::from_secs(1)),
            Ok(1)
        );
        assert_eq!(
            rx.recv_block_timeout(Instant::now() + Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    /// `recv_async_timeout`: value present succeeds; empty times out.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_recv_async_timeout() {
        wasm_lite_std::async_doctest!(async {
            let (tx, rx) = mpsc::channel();
            tx.send_async(1).await.unwrap();
            assert_eq!(
                rx.recv_async_timeout(Instant::now() + Duration::from_secs(1))
                    .await,
                Ok(1)
            );
            assert_eq!(
                rx.recv_async_timeout(Instant::now() + Duration::from_millis(10))
                    .await,
                Err(mpsc::RecvTimeoutError::Timeout)
            );
        });
    }

    /// `Debug` formatting of `Sender`/`Receiver`.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_debug() {
        let (tx, rx) = mpsc::channel::<i32>();
        assert_eq!(format!("{tx:?}"), "Sender");
        assert_eq!(format!("{rx:?}"), "Receiver");
    }

    /// `into_iter` yields each item a worker sends (blocking iteration).
    #[wasm_lite::wasm_lite_test(worker)]
    fn mpsc_into_iter() {
        let (tx, rx) = mpsc::channel();
        wasm_lite_std::spawn(move || {
            for i in 1..=3 {
                tx.send_block(i).unwrap();
            }
        });
        let mut iter = rx.into_iter();
        assert_eq!(iter.next(), Some(1));
        assert_eq!(iter.next(), Some(2));
        assert_eq!(iter.next(), Some(3));
        assert_eq!(iter.next(), None);
    }

    /// Dropping the sender still drains buffered values, then reports disconnect.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_disconnect_sender() {
        let (tx, rx) = mpsc::channel();
        tx.send_sync(1).unwrap();
        drop(tx);
        assert_eq!(rx.recv_sync(), Ok(1));
        assert_eq!(rx.recv_sync(), Err(mpsc::RecvError::Disconnected));
    }

    /// Sending on a channel whose receiver was dropped fails.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_disconnect_receiver() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        assert_eq!(tx.send_sync(1), Err(mpsc::SendError(1)));
    }

    /// `into_iter` over buffered values after the sender is dropped.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_iter_disconnect() {
        let (tx, rx) = mpsc::channel();
        tx.send_sync(1).unwrap();
        tx.send_sync(2).unwrap();
        tx.send_sync(3).unwrap();
        drop(tx);
        assert_eq!(rx.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    /// `recv_async` drains buffered values then reports disconnect.
    #[wasm_lite::wasm_lite_test]
    fn mpsc_disconnect_async() {
        wasm_lite_std::async_doctest!(async {
            let (tx, rx) = mpsc::channel();
            tx.send_async(1).await.unwrap();
            drop(tx);
            assert_eq!(rx.recv_async().await, Ok(1));
            assert_eq!(rx.recv_async().await, Err(mpsc::RecvError::Disconnected));
        });
    }

    // ---- Condvar (a notifier worker + a waiter worker, joined) ----

    /// `wait_spin` wakes after a notify.
    #[wasm_lite::wasm_lite_test(worker)]
    fn condvar_wait_spin() {
        run_condvar_wait(CondvarWait::Spin);
    }

    /// `wait_block` wakes after a notify.
    #[wasm_lite::wasm_lite_test(worker)]
    fn condvar_wait_block() {
        run_condvar_wait(CondvarWait::Block);
    }

    /// `wait_sync` wakes after a notify.
    #[wasm_lite::wasm_lite_test(worker)]
    fn condvar_wait_sync() {
        run_condvar_wait(CondvarWait::Sync);
    }

    /// `notify_all` wakes three `wait_sync` waiters.
    #[wasm_lite::wasm_lite_test(worker)]
    fn condvar_notify_all() {
        let pair = Arc::new((Mutex::new(0u32), Condvar::new()));
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let pair = Arc::clone(&pair);
                wasm_lite_std::spawn(move || {
                    let (m, cv) = &*pair;
                    let mut count = m.lock_sync();
                    while *count < 10 {
                        count = cv.wait_sync(count);
                    }
                    *count
                })
            })
            .collect();
        // Let the waiters reach their wait.
        spin_for(Duration::from_millis(20));
        let (m, cv) = &*pair;
        *m.lock_sync() = 10;
        cv.notify_all();
        for w in waiters {
            assert_eq!(w.join().unwrap(), 10);
        }
    }

    /// Producer pushes 5 items; consumer waits on the condvar and collects them.
    #[wasm_lite::wasm_lite_test(worker)]
    fn condvar_producer_consumer() {
        use std::collections::VecDeque;
        let shared = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let producer = Arc::clone(&shared);
        let p = wasm_lite_std::spawn(move || {
            let (m, cv) = &*producer;
            for i in 0..5 {
                m.lock_sync().push_back(i);
                cv.notify_one();
                spin_for(Duration::from_millis(2));
            }
        });
        let consumer = Arc::clone(&shared);
        let c = wasm_lite_std::spawn(move || {
            let (m, cv) = &*consumer;
            let mut collected = Vec::new();
            while collected.len() < 5 {
                let mut q = m.lock_sync();
                while q.is_empty() {
                    q = cv.wait_sync(q);
                }
                collected.push(q.pop_front().unwrap());
            }
            collected
        });
        p.join().unwrap();
        assert_eq!(c.join().unwrap(), vec![0, 1, 2, 3, 4]);
    }

    /// `wait_async_timeout` returns without a notify once the deadline passes.
    #[wasm_lite::wasm_lite_test]
    fn condvar_wait_async_timeout() {
        wasm_lite_std::async_doctest!(async {
            let m = Mutex::new(false);
            let cv = Condvar::new();
            let guard = m.lock_async().await;
            let deadline = Instant::now() + Duration::from_millis(20);
            let (_guard, timed_out) = cv.wait_async_timeout(guard, deadline).await;
            assert!(timed_out.timed_out());
        });
    }

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
}
