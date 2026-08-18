// SPDX-License-Identifier: MIT OR Apache-2.0
//! Observable state for threads spawned through `wasm_lite_std`.

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use crate::Mutex;
use crate::time::SystemTime;
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use std::sync::Arc;
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use std::sync::atomic::AtomicU64;
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use std::sync::atomic::{AtomicU8, Ordering};
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use std::sync::{OnceLock, Weak};

/// The lifecycle state visible for a spawned thread or Web Worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    /// The spawn request succeeded, but user code has not started executing.
    Spawned,
    /// The worker entered its Rust closure and is still executing it.
    Running,
    /// The closure returned normally and is finishing its thread teardown.
    Finished,
    /// The worker's panic hook ran before the worker trapped.
    Panicked,
}

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
impl ThreadState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Spawned,
            1 => Self::Running,
            2 => Self::Finished,
            3 => Self::Panicked,
            _ => unreachable!("invalid thread diagnostics state"),
        }
    }
}

/// A point-in-time view of one thread spawned through this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadInfo {
    /// Stable identifier within this process's diagnostics registry.
    pub id: u64,
    /// Name supplied through [`crate::Builder::name`], if any.
    pub name: Option<String>,
    /// Wall-clock time at which spawning was requested.
    pub spawned_at: SystemTime,
    /// Whether the worker is waiting to start, running, finishing, or panicked.
    pub state: ThreadState,
}

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
struct ThreadStatus {
    id: u64,
    name: Option<String>,
    spawned_at: SystemTime,
    state: AtomicU8,
}

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
fn registry() -> &'static Mutex<Vec<Weak<ThreadStatus>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<ThreadStatus>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Returns the threads whose spawn records are still live.
///
/// A Web Worker that has been created but has not reached its Rust closure is
/// [`ThreadState::Spawned`]. This distinction is the useful one in browsers: a
/// worker that never starts no longer looks like an idle executor. Completed
/// threads disappear after their spawn trampoline is released.
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
pub fn threads() -> Vec<ThreadInfo> {
    let mut registry = registry().lock_sync();
    let mut result = Vec::with_capacity(registry.len());
    registry.retain(|weak| {
        let Some(status) = weak.upgrade() else {
            return false;
        };
        result.push(ThreadInfo {
            id: status.id,
            name: status.name.clone(),
            spawned_at: status.spawned_at,
            state: ThreadState::from_u8(status.state.load(Ordering::Acquire)),
        });
        true
    });
    result.sort_by_key(|thread| thread.id);
    result
}

/// Returns an empty snapshot when thread spawning is unavailable.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
pub fn threads() -> Vec<ThreadInfo> {
    Vec::new()
}

#[derive(Clone)]
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
pub(crate) struct Registration(Arc<ThreadStatus>);

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
pub(crate) fn register(name: Option<String>) -> Registration {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let status = Arc::new(ThreadStatus {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        name,
        spawned_at: SystemTime::now(),
        state: AtomicU8::new(0),
    });
    registry().lock_sync().push(Arc::downgrade(&status));
    Registration(status)
}

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
impl Registration {
    pub(crate) fn running(&self) {
        self.0.state.store(1, Ordering::Release);
    }

    pub(crate) fn finished(&self) {
        self.0.state.store(2, Ordering::Release);
    }

    pub(crate) fn panicked(&self) {
        self.0.state.store(3, Ordering::Release);
    }
}

#[cfg(all(test, any(not(target_arch = "wasm32"), target_feature = "atomics")))]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn snapshots_follow_the_lifecycle_and_do_not_keep_threads_alive() {
        let registration = register(Some("renderer".to_string()));
        let id = registration.0.id;
        let find = || {
            threads()
                .into_iter()
                .find(|thread| thread.id == id)
                .unwrap()
        };

        assert_eq!(find().state, ThreadState::Spawned);
        registration.running();
        assert_eq!(find().state, ThreadState::Running);
        registration.finished();
        assert_eq!(find().state, ThreadState::Finished);
        drop(registration);
        assert!(threads().into_iter().all(|thread| thread.id != id));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test(worker))]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn spawned_threads_are_registered_while_running() {
        let name = "wasm-lite-diagnostics-test".to_string();
        let (entered_sender, entered_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let handle = crate::Builder::new()
            .name(name.clone())
            .spawn(move || {
                entered_sender.send(()).unwrap();
                release_receiver.recv().unwrap();
            })
            .unwrap();

        entered_receiver.recv().unwrap();
        let observed = threads()
            .into_iter()
            .find(|thread| thread.name.as_deref() == Some(&name))
            .unwrap();
        assert_eq!(observed.state, ThreadState::Running);

        release_sender.send(()).unwrap();
        handle.join().unwrap();
        assert!(
            threads()
                .into_iter()
                .all(|thread| thread.name.as_deref() != Some(&name))
        );
    }
}
