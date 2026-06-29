// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// A thread local storage key which owns its contents.
///
/// This wraps `std::thread::LocalKey` which provides thread-local storage
/// on WASM with atomics via the standard library's TLS implementation.
pub struct LocalKey<T: 'static> {
    inner: &'static std::thread::LocalKey<T>,
}

impl<T: 'static> LocalKey<T> {
    /// Creates a new `LocalKey`.
    #[doc(hidden)]
    pub const fn new(inner: &'static std::thread::LocalKey<T>) -> Self {
        LocalKey { inner }
    }

    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this is the first time
    /// the current thread has called `with` on this key.
    ///
    /// # Panics
    ///
    /// This function will panic if the initialization function panics.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.inner.with(f)
    }

    /// Acquires a reference to the value in this TLS key.
    ///
    /// Returns `Err(AccessError)` if the key is being destroyed or
    /// was already destroyed.
    pub fn try_with<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        self.inner.try_with(f).map_err(|_| AccessError)
    }
}

impl<T: 'static> fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalKey").finish_non_exhaustive()
    }
}

/// An error returned by [`LocalKey::try_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccessError;

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "already destroyed or being destroyed")
    }
}

impl std::error::Error for AccessError {}

/// A handle to a thread.
pub struct JoinHandle<T> {
    receiver: crate::mpsc::Receiver<Result<T, String>>,
    thread: Thread,
    finished: Arc<AtomicBool>,
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle")
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

impl<T> JoinHandle<T> {
    /// Waits for the thread to finish and returns its result.
    ///
    /// Unfortunately, this is almost never what you want, and is likely to prevent your workers from spawning.
    /// Consider [`JoinHandle::join_async`] instead, or see the documentation on [`crate::spawn`] for details.
    pub fn join(self) -> Result<T, Box<String>> {
        if is_main_thread() {
            return Err(Box::new(
                "Can't join from the main thread on wasm32".to_string(),
            ));
        }
        self.receiver
            .recv_sync()
            .map_err(|e| Box::new(format!("{:?}", e)) as Box<String>)?
            .map_err(|e| Box::new(e) as Box<String>)
    }

    /// Waits asynchronously for the thread to finish and returns its result.
    ///
    /// This is the async version of [`JoinHandle::join`]. The error type differs
    /// from the synchronous version - panics are converted to `Box<String>` containing
    /// the debug representation of the panic payload.
    pub async fn join_async(self) -> Result<T, Box<String>> {
        // The result arrives over the channel, which carries Result<T, String>
        // to support panic propagation. (The wasm_lite worker frees its own
        // stack/TLS and self-closes, so there is no separate exit handshake.)
        self.receiver
            .recv_async()
            .await
            .map_err(|e| Box::new(format!("{:?}", e)) as Box<String>)?
            .map_err(|e| Box::new(e) as Box<String>)
    }

    /// Gets the thread associated with this handle.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Checks if the thread has finished running.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

/// A handle to a thread.
#[derive(Clone)]
pub struct Thread {
    inner: Arc<ThreadInner>,
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("id", &self.inner.id)
            .field("name", &self.inner.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for Thread {}

impl std::hash::Hash for Thread {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state);
    }
}

struct ThreadInner {
    name: Option<String>,
    id: ThreadId,
    /// Parking state: 0 = no token, 1 = unpark token present.
    /// This is a Box'd atomic so we get a stable address in wasm linear memory.
    parking_state: Box<AtomicU32>,
}

impl Thread {
    /// Gets the thread's unique identifier.
    pub fn id(&self) -> ThreadId {
        self.inner.id
    }

    /// Gets the thread's name.
    pub fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// Atomically makes the handle's token available if it is not already.
    pub fn unpark(&self) {
        let ptr = self.inner.parking_state.as_ref() as *const AtomicU32 as u32;
        park_notify_at_addr(ptr);
    }
}

/// A unique identifier for a running thread.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ThreadId(u64);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ThreadId({})", self.0)
    }
}

/// A builder for configuring and spawning threads.
#[derive(Debug)]
pub struct Builder {
    _name: Option<String>,
    _stack_size: Option<usize>,
    _shim_name: Option<String>,
}

impl Builder {
    /// Creates a new thread builder.
    pub fn new() -> Self {
        Builder {
            _name: None,
            _stack_size: None,
            _shim_name: None,
        }
    }

    /// Set the wasm-bindgen shim name for worker spawning.
    /// This should match the binary/example name (e.g., "single_spawn", "wasm-bindgen-test").
    pub fn shim_name(mut self, name: String) -> Self {
        self._shim_name = Some(name);
        self
    }

    /// Sets the name of the thread.
    pub fn name(mut self, name: String) -> Self {
        self._name = Some(name);
        self
    }

    /// Sets the stack size for the new thread.
    pub fn stack_size(mut self, size: usize) -> Self {
        self._stack_size = Some(size);
        self
    }

    /// Spawns a new thread with the configured settings.
    ///
    /// # Platform Notes (WASM)
    ///
    /// **The spawned thread will not begin executing until you yield to the JavaScript
    /// event loop.** On WASM targets, worker threads are spawned asynchronously via
    /// the JS event loop. When this function returns, the worker has been *created*
    /// but has not yet *started*.
    ///
    /// You must yield to the event loop to allow the worker to begin:
    ///
    /// ```
    /// let handle = wasm_lite_std::Builder::new().spawn(|| { /* ... */ }).unwrap();
    /// # async fn ex() {
    /// wasm_lite_std::yield_to_event_loop_async().await;  // Worker starts here, not above!
    /// # }
    /// ```
    ///
    /// Failure to yield can cause:
    /// - `is_finished()` to always return `false`
    /// - Deadlocks when waiting on atomics the worker should have set
    /// - Tests that hang indefinitely
    ///
    /// For more details on this problem and why it is hard, see implementation bugs:
    /// * <https://issues.chromium.org/issues/40633395>
    /// * <https://bugzilla.mozilla.org/show_bug.cgi?id=1888109>
    /// * <https://bugs.webkit.org/show_bug.cgi?id=271756>
    pub fn spawn<F, T>(self, f: F) -> io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // Create Thread before spawning so we can pass it to the worker
        // Thread::name() only returns Some if explicitly set via Builder::name()
        let id = ThreadId(THREAD_COUNTER.fetch_add(1, Ordering::Relaxed));
        let thread = Thread {
            inner: Arc::new(ThreadInner {
                name: self._name.clone(),
                id,
                parking_state: Box::new(AtomicU32::new(0)),
            }),
        };

        let thread_for_worker = thread.clone();

        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_worker = finished.clone();

        let (send, recv) = crate::mpsc::channel();
        let closure = move || {
            // Mark this worker so is_main_thread()/atomic.wait gating works here.
            mark_worker_thread();

            // Set up TLS for current() before running user code
            CURRENT_THREAD.with(|cell| {
                *cell.borrow_mut() = Some(thread_for_worker);
            });

            // Set up panic handling: store a sender closure that the panic hook can use
            // to send the error through the channel before aborting
            PANIC_SENDER.with(|cell| {
                let send_clone = send.clone();
                let finished_clone = finished_for_worker.clone();
                *cell.borrow_mut() = Some(Box::new(move |msg: String| {
                    finished_clone.store(true, Ordering::Release);
                    let _ = send_clone.send_sync(Err(msg));
                }));
            });

            // Install our panic hook (once, globally — it reads thread-locals,
            // so a single hook serves every thread).
            install_panic_hook();

            crate::hooks::run_spawn_hooks();

            let result = f();

            // Clear panic sender since we completed successfully
            PANIC_SENDER.with(|cell| {
                cell.borrow_mut().take();
            });

            flush_captured_prints_to_console_current_thread_impl();

            // Mark as finished before sending result (Release pairs with Acquire in is_finished)
            finished_for_worker.store(true, Ordering::Release);
            // Ignore send errors - receiver may have been dropped if JoinHandle wasn't joined
            let _ = send.send_sync(Ok(result));
        };

        // Hand the closure to wasm_lite's spawn primitive, which boxes it,
        // starts a Web Worker sharing this module + memory, and runs it on a
        // fresh stack/TLS. (Worker name / stack size are not yet plumbed
        // through; they are accepted for API compatibility.)
        wasm_lite::thread::spawn(closure);

        Ok(JoinHandle {
            receiver: recv,
            thread,
            finished,
        })
    }
}

/// Install the wasm_lite_std panic hook — **once**, globally.
///
/// The hook reads thread-locals ([`CURRENT_THREAD`], [`PANIC_SENDER`]), so one
/// hook serves every thread; installing it per-spawn would nest hooks and
/// multiply the output. It is the single canonical logger and deliberately does
/// **not** chain to a prior hook: the default wasm32 panic prints nothing, so we
/// always log the panic to the console with thread attribution (a panic only
/// traps its own worker, so a detached worker's panic would otherwise be silent),
/// and route it to the join channel when this thread has a sender. Consequence:
/// once you spawn a thread, wasm_lite_std owns the panic hook — install any
/// custom hook (`set_panic_hook`, etc.) before the first spawn.
fn install_panic_hook() {
    use std::sync::Once;
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let msg = info.to_string();
            let who = CURRENT_THREAD
                .with(|c| {
                    c.borrow().as_ref().map(|t| match t.name() {
                        Some(name) => name.to_string(),
                        None => t.id().to_string(),
                    })
                })
                .unwrap_or_else(|| {
                    if is_main_thread() { "main".to_string() } else { "unknown thread".to_string() }
                });
            wasm_lite::console::error(&format!("[wasm_lite_std {who}] {msg}"));

            // Deliver the panic to a `join`er, if this thread has a sender.
            PANIC_SENDER.with(|cell| {
                if let Some(sender) = cell.borrow_mut().take() {
                    flush_captured_prints_to_console_current_thread_impl();
                    sender(msg);
                }
            });
        }));
    });
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawns a new thread, returning a [`JoinHandle`] for it.
///
/// # Warning (WASM)
///
/// **The thread will not start until you yield to the JS event loop.**
/// See [`Builder::spawn`] for details.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Builder::new().spawn(f).expect("failed to spawn thread")
}

/// Gets a handle to the thread that invokes it.
pub fn current() -> Thread {
    CURRENT_THREAD.with(|cell| {
        let mut borrowed = cell.borrow_mut();
        if let Some(ref thread) = *borrowed {
            thread.clone()
        } else {
            // Lazily create a Thread for threads we didn't spawn
            let name = if is_main_thread() {
                Some("main".to_string())
            } else {
                // Thread not spawned by us (e.g., a Web Worker created externally)
                None
            };
            let id = ThreadId(THREAD_COUNTER.fetch_add(1, Ordering::Relaxed));
            let thread = Thread {
                inner: Arc::new(ThreadInner {
                    name,
                    id,
                    parking_state: Box::new(AtomicU32::new(0)),
                }),
            };
            *borrowed = Some(thread.clone());
            thread
        }
    })
}

/// Puts the current thread to sleep for at least the specified duration.
pub fn sleep(dur: Duration) {
    sleep_sync_ms(dur.as_millis() as f64);
}

/// Cooperatively gives up a timeslice to the OS scheduler.
pub fn yield_now() {
    atomics_wait_timeout_ms_try(0.001);
}

/// Yields to the event loop, allowing pending tasks (like worker startup) to run.
///
/// Returns `Pending` exactly once, so the [`spawn_local`](super::spawn_local)
/// executor reschedules the caller after a turn of the event loop.
pub async fn yield_to_event_loop_async() {
    struct YieldOnce(bool);
    impl std::future::Future for YieldOnce {
        type Output = ();
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            if self.0 {
                std::task::Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }
    YieldOnce(false).await
}

/// Blocks unless or until the current thread's token is made available.
pub fn park() {
    let thread = current();
    let ptr = thread.inner.parking_state.as_ref() as *const AtomicU32 as u32;
    if park_wait_at_addr(ptr) == WaitResult::Unsupported {
        panic!("atomic.wait is not available in this context (likely main thread)");
    }
}

/// Blocks unless or until the current thread's token is made available
/// or the specified duration has been reached.
pub fn park_timeout(dur: Duration) {
    let thread = current();
    let ptr = thread.inner.parking_state.as_ref() as *const AtomicU32 as u32;
    if park_wait_timeout_at_addr(ptr, dur.as_millis() as f64) == WaitResult::Unsupported {
        panic!("atomic.wait is not available in this context (likely main thread)");
    }
}

/// Returns an estimate of the default amount of parallelism a program should use.
pub fn available_parallelism() -> io::Result<NonZeroUsize> {
    let count = get_available_parallelism();
    NonZeroUsize::new(count as usize).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine available parallelism",
        )
    })
}
