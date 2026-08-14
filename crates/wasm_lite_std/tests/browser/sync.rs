// SPDX-License-Identifier: MIT OR Apache-2.0
//! Synchronization primitives: `Spinlock`, `Mutex`, `mpsc`, and `Condvar`.
//!
//! Naming: tests whose body must block (`*_block`, `recv_block`, blocking
//! `into_iter`, deadlines elapsing while another thread holds the lock) use
//! `#[wasm_lite_test(worker)]` so `Atomics.wait` is available; non-blocking /
//! async / uncontended ones run on the main thread. Cross-thread coordination
//! uses `join()` / `mpsc`.

use super::{CondvarWait, run_condvar_wait, spin_for};
use std::sync::Arc;
use wasm_lite_std::condvar::Condvar;
use wasm_lite_std::spinlock::Spinlock;
use wasm_lite_std::time::{Duration, Instant};
use wasm_lite_std::{Mutex, mpsc};

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
    let (release_tx, release_rx) = mpsc::channel();
    let m2 = Arc::clone(&m);
    wasm_lite_std::spawn(move || {
        let _g = m2.lock_spin();
        tx.send_block(()).unwrap(); // acquired
        release_rx.recv_block().unwrap(); // held until told otherwise
    });
    rx.recv_block().unwrap();
    let deadline = Instant::now() + Duration::from_millis(10);
    // A fixed hold duration raced here: the assertion only means something
    // while the lock is actually held, and a slow wake put the attempt
    // after the release. See `mutex_lock_block_timeout`.
    assert!(m.lock_spin_timeout(deadline).is_none());
    release_tx.send_block(()).unwrap();
}

/// `lock_block_timeout`: `None` while held, then `Some` after release.
///
/// The holder waits to be told to release rather than holding for a fixed
/// 50 ms. With a duration this raced: the assertion below only means
/// something while the lock is *actually* held, and under Chrome the wake
/// from `Atomics.wait` can take longer than the margin, so the attempt
/// sometimes landed after the holder had already let go and the test failed
/// claiming `lock_block_timeout` had succeeded when it should not. A
/// handshake removes the assumption instead of widening it.
#[wasm_lite::wasm_lite_test(worker)]
fn mutex_lock_block_timeout() {
    let m = Arc::new(Mutex::new(0));
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let m2 = Arc::clone(&m);
    wasm_lite_std::spawn(move || {
        let g = m2.lock_block();
        tx.send_block(()).unwrap(); // acquired
        release_rx.recv_block().unwrap(); // held until told otherwise
        drop(g);
        tx.send_block(()).unwrap(); // released
    });
    rx.recv_block().unwrap();
    assert!(
        m.lock_block_timeout(Instant::now() + Duration::from_millis(10))
            .is_none()
    );
    release_tx.send_block(()).unwrap();
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
    let (release_tx, release_rx) = mpsc::channel();
    let m2 = Arc::clone(&m);
    wasm_lite_std::spawn(move || {
        let _g = m2.lock_sync();
        tx.send_block(()).unwrap();
        release_rx.recv_block().unwrap(); // held until told otherwise
    });
    rx.recv_block().unwrap();
    let deadline = Instant::now() + Duration::from_millis(10);
    // A fixed hold duration raced here: the assertion only means something
    // while the lock is actually held, and a slow wake put the attempt
    // after the release. See `mutex_lock_block_timeout`.
    assert!(m.lock_sync_timeout(deadline).is_none());
    release_tx.send_block(()).unwrap();
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

        // The holder waits to be told to release. Holding for a fixed
        // 100 ms raced the same way `mutex_lock_block_timeout` did: the
        // assertion below only means something while the lock is actually
        // held, and a slow wake put the attempt after the release.
        let (tx, mut rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let m2 = Arc::clone(&m);
        wasm_lite_std::spawn(move || {
            let _g = m2.lock_block();
            tx.send_block(()).unwrap();
            release_rx.recv_block().unwrap();
        });
        rx.recv_async().await.unwrap();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(m.lock_async_timeout(deadline).await.is_none());
        // `send_spin`, not `send_block`: this runs on the browser main
        // thread, where `Atomics.wait` is forbidden.
        release_tx.send_spin(()).unwrap();
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
        let (tx, mut rx) = mpsc::channel();
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
        let (tx, mut rx) = mpsc::channel();
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
        let (tx, mut rx) = mpsc::channel();
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
