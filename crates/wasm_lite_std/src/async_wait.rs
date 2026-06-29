// SPDX-License-Identifier: MIT OR Apache-2.0
use atomic_waker::AtomicWaker;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};

const WAITING: u8 = 0;
const WOKEN: u8 = 1;
const CANCELED: u8 = 2;

pub(crate) fn waiter() -> (AsyncWake, AsyncWait) {
    let inner = Arc::new(Inner {
        state: AtomicU8::new(WAITING),
        waker: AtomicWaker::new(),
    });
    (
        AsyncWake {
            inner: Arc::clone(&inner),
        },
        AsyncWait { inner },
    )
}

struct Inner {
    state: AtomicU8,
    waker: AtomicWaker,
}

pub(crate) struct AsyncWake {
    inner: Arc<Inner>,
}

impl AsyncWake {
    pub(crate) fn wake(self) -> bool {
        if self
            .inner
            .state
            .compare_exchange(WAITING, WOKEN, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.inner.waker.wake();
            true
        } else {
            false
        }
    }
}

impl fmt::Debug for AsyncWake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncWake").finish_non_exhaustive()
    }
}

pub(crate) struct AsyncWait {
    inner: Arc<Inner>,
}

impl Future for AsyncWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.inner.state.load(Ordering::Acquire) == WOKEN {
            return Poll::Ready(());
        }

        self.inner.waker.register(cx.waker());

        if self.inner.state.load(Ordering::Acquire) == WOKEN {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for AsyncWait {
    fn drop(&mut self) {
        let _ = self.inner.state.compare_exchange(
            WAITING,
            CANCELED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl fmt::Debug for AsyncWait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncWait").finish_non_exhaustive()
    }
}
