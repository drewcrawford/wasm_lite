# Threads, async & shared memory

*(Part of the [wasm_lite](../README.md) docs. See also: [binding model](./binding-model.md),
[testing](./testing.md), [interop](./interop.md), [roadmap](./roadmap.md),
[migration guide](../MIGRATION.md).)*

## Threading goals

Atomics, workers, and std-like concurrency are first-class targets for
wasm_lite. The main target is still the browser, so the design starts from
browser constraints instead of pretending `std::thread` can be dropped in
unchanged:

* the browser main thread cannot block on `Atomics.wait`;
* shared memory requires `SharedArrayBuffer` and cross-origin isolation headers;
* workers are separate JS realms, so logs and value handles need explicit
  routing;
* a worker that returns to JS can disappear while Rust async tasks still live in
  its TLS unless the bootstrap drains them deliberately.

[wasm-bindgen](https://wasm-bindgen.github.io/wasm-bindgen/)'s own
[threaded example](https://wasm-bindgen.github.io/wasm-bindgen/examples/raytrace.html)
documents several consequences of making threads fit a broad target matrix:
threaded code needs specific output targets (`web` or `no-modules` in that
guide), bundler output is not generally supported for that path, worker shims
are hand-shaped, and there is no standard `std::thread`-like model. wasm_lite
narrows the target to modern browsers so the implementation can own the whole
path: shared memory creation, module-worker startup, TLS/stack setup, async
draining, COOP/COEP serving, logging, and test capture.

## Shared memory & atomics

wasm_lite runs modules built with the threads-related wasm features
(`+atomics,+bulk-memory,+mutable-globals`) on a **shared** linear memory (a
`SharedArrayBuffer`) — the foundation for threads (`wasm_lite::thread::spawn` and
the `wasm_lite_std` layer above it). Everything below is in place:

* **Toolchain.** `+atomics` means `std` must be recompiled with it, so these
  builds need **nightly** and `-Z build-std`. See
  `examples/atomics-demo/.cargo/config.toml`: it sets the target features, links
  with `--shared-memory --max-memory=… --import-memory`, and adds
  `build-std = ["std", "panic_abort"]`. Build with `cargo +nightly run`.
* **Imported memory.** `--import-memory` makes the module import its memory
  rather than define it, so JS owns the one `WebAssembly.Memory` object (the same
  object every future worker will share). The codegen reads the module's imported
  memory limits and emits `makeMemory()` plus an `instantiate(url, memory?)` that
  creates the shared memory (or accepts one) and supplies it as an import.
* **Cross-origin isolation.** Browsers only hand out `SharedArrayBuffer` to
  cross-origin-isolated pages, so the runner serves
  `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp` on every response.
* **Init.** LLD emits a `start` function that sets up the main thread's TLS and
  initializes passive data segments on first instantiation — so single-threaded
  atomic code and `thread_local!` work with no manual setup.

`JsValue` is already `!Send`/`!Sync`: a handle indexes a per-realm value table,
so it is only valid on the worker that created it — the type system forbids
sending one across threads.

## Spawning threads

`wasm_lite::thread::spawn(move || { … })` runs a closure on a new Web Worker
sharing this module's compiled `WebAssembly.Module` and shared memory — **no
wasm-bindgen, js-sys, or web-sys**. The `Worker` lives entirely in the generated
glue behind a single `__wl_spawn` import:

* `spawn` boxes the closure (double-boxed to a thin `u32` pointer) and calls
  `__wl_spawn`.
* The glue allocates a fresh stack + TLS block (`__wl_thread_alloc`) and starts a
  worker, postMessaging `{ module, memory, work, stackTop, tlsPtr }`.
* The worker (a codegen-emitted bootstrap, `wl_worker.js`) instantiates the same
  module on the same memory, points `__stack_pointer` at the new stack, calls
  `__wasm_init_tls`, then `__wl_thread_entry` — which reconstitutes the closure
  and runs it. Threads coordinate via `core::sync::atomic`.

This requires the worker bootstrap to import the glue *without* re-running
`main`, so the runner serves the glue (`program.js`) separately from a small
bootstrap module. A threaded build must export the linker's thread symbols;
`examples/threads-demo/.cargo/config.toml` shows the flags
(`--export=__stack_pointer`, `__tls_size`, `__wasm_init_tls`, …).

The core `spawn` is **detached** (no `JoinHandle`). The std-like layer with
`spawn -> JoinHandle`, `park`/`unpark`, `Mutex`/`Condvar`/`RwLock`/`mpsc` lives in
`wasm_lite_std` (a port of `wasm_safe_thread`, retargeted off wasm-bindgen onto
this primitive + `core::arch::wasm32` atomics).

Both the **sync** and **async** paths work. Since the main thread can't block,
`wasm_lite_std` ships a small event-loop executor: `spawn_local(future)` runs a
task on the event loop, and while it's pending the executor *sleeps* on
`Atomics.waitAsync` (the `__wl_wait_async` runtime import) rather than busy-polling.
Wakes are edge-triggered and cross-thread: each executor owns a wake atom, and a
task's `Waker` bumps it and issues `memory.atomic.notify` — which resolves the
owning realm's `waitAsync` Promise even when the notify comes from another worker.
So `JoinHandle::join_async().await`, `Mutex::lock_async().await`, etc. run
non-blocking on the main thread and are woken the instant a worker releases.

## Async lifecycle & failures — two fixes for wasm-bindgen footguns

`spawn_local` is meant to be the **uniform** primitive: the same
`spawn_local(a); spawn_local(b); …` works on any thread, and "wait for my tasks"
is implicit — the event loop on the main thread, a drain refcount on a worker.
`block_on` is the niche tool (a worker that truly needs a *synchronous* result);
it is the one that must know it isn't the main thread, and deadlocks if it's
wrong. For that uniformity to hold, two things have to be true:

* **Threads drain their async tasks before teardown.** A wasm-bindgen worker
  `close()`s when its entry returns, so a `spawn_local`'d task is silently
  abandoned — "the thread shut down and my futures mysteriously stopped" — and its
  TLS (where the task queue lives) is freed underneath it. Instead the worker
  bootstrap polls the exported `__wl_executor_idle()` and only frees its TLS/stack
  + `close()`s once the executor has drained, so `spawn_local` is correct on *any*
  thread, not just main (proven in `examples/worker-spawn-local-demo`). Residual
  hazard: a worker task that never completes keeps the worker alive — explicit
  termination is rare, and the right tool for it is *cooperative cancellation* (a
  token the tasks check), not a hard `terminate()` that strands held locks.

* **Async tests are fail-closed** — `wasm_lite_std::async_doctest!(async { … })`
  (usable in doctests, `#[wasm_lite_test]` bodies, and `main`). Unlike
  `rustdoc`/`libtest`, where `main` returning *is* the verdict (so a deferred
  async failure can't be seen), the body marks itself pending so the verdict is
  deferred; the *only* thing that records success is the body reaching its end.
  A panic, dropped task, or deadlock cannot masquerade as a pass: a panic in a
  polled task traps the executor tick, which a `try/catch` turns into
  `{ok:false}` (with the message via the captured console), and a hang falls to
  the runner's timeout. The verdict is rendered by the runner polling a
  still-live browser page, not by `main` returning — which is what makes
  deferring it possible. (Caveat: rustdoc links doctests with `rustdocflags`, not
  `rustflags`, so an async doctest crate must repeat the threads/atomics link
  args under `[build] rustdocflags` — see `examples/async-doctest-demo`.)

`wasm_lite_std` installs a single canonical panic hook (once, on first spawn) that
logs each panic exactly once with thread attribution and routes it to the join
channel — so it owns the panic hook for threaded programs; install any custom hook
(`set_panic_hook`) *before* the first spawn.

On panics: `panic = "abort"` is the supported model. On wasm a panic is an
`unreachable` **trap local to one instance** — verified: a panicking worker traps
only itself; the main thread and other workers keep running and shared memory
persists (unlike native, where `abort()` kills the whole process). So in a
multithreaded executor a panic takes down only *that* worker (its futures die with
it); siblings are unaffected. The one residual surprise is a lock the dead thread
held — with no unwind there's no `Drop` and no poison, so it stays locked — which
is why the `*_timeout` lock APIs exist: a dead holder surfaces as a timeout, not a
hang. (Our own runtime holds no cross-thread lock across a poll — the executor's
queue is thread-local and its wake is a lock-free atom — and the shared allocator
survives a panic, so a no-user-lock hang is not something we introduce. A future
`panic = "unwind"` mode could `catch_unwind` per poll, drop just the failed task,
and poison its locks.)

## How a panic surfaces (browser vs CLI)

The default `wasm32` panic prints nothing, so `wasm_lite_std`'s worker hook
**always** logs the panic to the console with thread attribution
(`[wasm_lite_std ThreadId(N)] panicked at …`) — never silent — *in addition to*
routing it to the join channel. That covers the interactive/browser case fully.

The CLI (`cargo test` / doctests via the runner) surfaces panics as follows:

| Panic site | Browser console | CLI (terminal) |
|---|---|---|
| main thread, hook installed | ✓ message | ✓ message + `FAILED` (runner prints the captured console on failure) |
| main thread, no hook | trap only | trap only — install `set_panic_hook()` |
| **joined** worker | ✓ message | ✓ fails: via the channel → the joiner re-panics on main → captured |
| **detached** worker | ✓ message | ⚠ message only — bridged up as a warning; does **not** fail the test |

A worker is a separate JS realm, so the runner can't read its console directly.
Instead worker console output is **bridged to the main realm**: each worker
forwards its console lines up the spawn chain via `postMessage`, and the runner
prints any worker-panic lines. So a *detached* worker panic does reach the
terminal — as a warning, e.g. `[wasm_lite_std ThreadId(0)] panicked at …` — even
though, matching `std`, an unjoined worker's panic doesn't fail the test (see
**Detached vs. awaited** below). A *joined* worker panic, by contrast, travels
the join channel and re-panics on the awaiter, which *does* fail the test.

**Detached vs. awaited.** A *detached* (never-joined) worker panic is logged but
doesn't fail the test — matching `std`, where an unjoined thread's panic prints
without failing. An **awaited** panic *propagates*: the worker's panic is
delivered to `join_async().await` as `Err(message)` (sent through the channel
before the worker aborts), so a wrapper returning `T` unwraps it and re-panics on
the awaiter — failing the test, exactly like `std::thread::join` /
`tokio::JoinHandle` (which hand you a `Result` you unwrap). When that await runs on
the main-thread executor (the usual case), wrap it in `async_doctest!` so the
re-panic becomes a hard CLI **failure** with the message (proven in
`examples/async-fail-demo`) rather than a passing-with-warning — the same
fail-closed machinery that makes async doctests trustworthy.

Doctests go through the same path, so they inherit all of the above. A failing
*sync* doctest with `set_panic_hook()` reports the full message + `FAILED` on the
CLI. Note: with Rust 2024 *merged* doctests, the first `panic = "abort"` aborts
the whole bundle, so later doctests in the crate don't run.

## `std::time` veneer

`wasm_lite_std::time` is a cross-platform [`std::time`] replacement, mirroring the
threading API: on native it re-exports the real `std::time` types; on wasm32 it
provides drop-in `Instant` and `SystemTime` backed by the browser clocks
(`performance.now()` and `Date.now()`, via `wasm_lite::performance`/`wasm_lite::date`)
— with **no** `wasm-bindgen`/`js-sys` dependency (unlike [`web-time`]). `Duration`
is re-exported unchanged. `Instant` is stored as a `Duration` from its time origin
so it is `Eq`/`Ord`/`Hash` like the real thing; `SystemTime` cannot represent
instants before the Unix epoch (arithmetic past `UNIX_EPOCH` returns `None`).

`wasm_lite_std::is_main_thread()` rounds out the threading surface: `true` on the
browser main thread (and the native process's initial thread), `false` on a
spawned worker — the thread where `Atomics.wait` (blocking locks, `park`) is
unavailable.

[`std::time`]: https://doc.rust-lang.org/std/time/
[`web-time`]: https://crates.io/crates/web-time
