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

There are two supported wasm modes:

* **Stable, single-realm wasm** uses ordinary non-shared memory. `spawn_local`,
  `JsFuture`, `sleep_async`, and the non-blocking/spinning synchronization paths
  work without atomics, `SharedArrayBuffer`, cross-origin isolation, or
  `-Z build-std`. Actual thread spawning is unavailable in this mode;
  `Builder::spawn` returns `io::ErrorKind::Unsupported`.
* **Shared-memory wasm** enables atomics and Web Workers. It adds real
  `spawn`/`JoinHandle`, cross-thread wakes, and blocking primitives on workers,
  at the cost of the nightly/build-std and browser-isolation requirements below.

The [wasm-bindgen threaded example](https://wasm-bindgen.github.io/wasm-bindgen/examples/raytrace.html)
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
* The worker (a codegen-emitted bootstrap, `wl_worker.js` in the runner or
  `<glue>.worker.js` from `wasm_lite -o <glue>`) instantiates the same
  module on the same memory, points `__stack_pointer` at the new stack, calls
  `__wasm_init_tls`, then `__wl_thread_entry` — which reconstitutes the closure
  and runs it. Threads coordinate via `core::sync::atomic`.
* When the worker reports completion and closes, the parent realm frees its
  stack and TLS. The worker cannot safely free the stack it is still returning
  through or its active TLS.

### Workers are always created by the main thread

The allocation above happens on whichever thread called `spawn`, but the
`new Worker(…)` does not: if the caller is itself a worker, the glue posts the
already-allocated pointers to its creator, which creates the worker. One
`postMessage` per spawn.

Because of that rule, the main thread ends up creating *every* worker, so every
worker's creator **is** the main thread and the hop is always exactly one — there
is no chain to walk and no way for an intermediate parent to have exited first.
Nesting depth in Rust is unbounded; the messaging is flat.

That indirection is not tidiness. **Chrome fetches a nested worker's module
script through its parent, and a parent sitting in `Atomics.wait` never services
that fetch** — so the child never starts, and a parent that goes on to `join`,
`park`, `recv_block` or `lock_block` waits forever. Every blocking primitive sits
in `Atomics.wait`, which made "spawn a thread, then wait for it" — the most
ordinary thing a worker can do — a guaranteed deadlock. Nothing reported an
error: the spawn returned a handle and the thread silently never ran.

Firefox does not behave this way, so a Firefox-only test run stays green while
Chrome hangs. `scripts/wasm32/tests` therefore runs the `wasm_lite_std` browser
suite in **both** browsers, and `a_worker_can_spawn_a_worker` /
`nesting_composes_to_a_third_level` exist to keep it that way.

The main thread's event loop is always turning, so routing creation there
sidesteps the problem entirely.

This requires the worker bootstrap to import the glue *without* re-running
`main`, so the runner serves the glue (`program.js`) separately from a small
bootstrap module.

## The threaded build configuration

A threaded build must export the linker's thread symbols. `wasm-ld` exports none
of them on its own — not even in a `--shared-memory` build — and a link line that
omits one still links and still spawns, so the mistake surfaces at runtime or
not at all.

This is the canonical block. Copy it whole; the failure mode for copying part of
it is bad (see below), and it is the reason this listing exists in one place
rather than only in the ten example `config.toml`s.

The five exports are wanted by two different consumers, and only three of them by
anything in this project — see [Which of the five you actually
need](#which-of-the-five-you-actually-need) before pruning the list, because the
other two are load-bearing for interop builds in a way the generated JavaScript
gives no hint of.

```toml
# .cargo/config.toml — threaded wasm32 (nightly: atomics ⇒ std is rebuilt)
[build]
target = "wasm32-unknown-unknown"
rustflags = [
    "-C", "target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C", "link-arg=--shared-memory",
    "-C", "link-arg=--max-memory=1073741824",
    "-C", "link-arg=--import-memory",
    "-C", "link-arg=--export=__stack_pointer",
    "-C", "link-arg=--export=__tls_base",
    "-C", "link-arg=--export=__tls_size",
    "-C", "link-arg=--export=__tls_align",
    "-C", "link-arg=--export=__wasm_init_tls",
]

# Doctests are linked by rustdoc, which does not read `rustflags`. This section
# must be keyed by the *exact triple*: rustdoc ignores `rustdocflags` under a
# `cfg(...)` predicate, so `[target.'cfg(target_arch = "wasm32")']` silently
# does nothing here even though it works for `rustflags`.
[target.wasm32-unknown-unknown]
rustdocflags = [
    "-C", "target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C", "link-arg=--shared-memory",
    "-C", "link-arg=--max-memory=1073741824",
    "-C", "link-arg=--import-memory",
    "-C", "link-arg=--export=__stack_pointer",
    "-C", "link-arg=--export=__tls_base",
    "-C", "link-arg=--export=__tls_size",
    "-C", "link-arg=--export=__tls_align",
    "-C", "link-arg=--export=__wasm_init_tls",
]

[unstable]
build-std = ["std", "panic_abort"]
```

Nothing can supply these for you: cargo does not propagate `rustc-link-arg` from
a dependency's build script to a dependent's binaries, and doctests are linked by
rustdoc, which build scripts do not reach at all. So `wasm_lite build` and
`wasm_lite run` check the compiled module instead and refuse to generate glue for
a module that spawns threads without them, naming the flags.

### Which of the five you actually need

Two different consumers want these symbols, which is why the list looks longer
than the runtime appears to justify:

| symbol | read by | needed when |
|---|---|---|
| `__stack_pointer` | the worker bootstrap, to point the new thread's stack | always |
| `__wasm_init_tls` | the worker bootstrap, to initialize the thread's TLS | always |
| `__tls_size` | the spawning side, to size the TLS block | always |
| `__tls_base` | **the wasm-bindgen CLI's** threading transform | interop builds |
| `__tls_align` | **the wasm-bindgen CLI's** threading transform | interop builds |

A module with no wasm-bindgen anywhere in its wasm32 dependency graph genuinely
runs on the first three: it spawns and joins threads correctly without the other
two, which the generated JavaScript never mentions. Add wasm-bindgen and the CLI
runs its own threading pass over the module, which stops with `failed to find
__tls_align` — and then, once that is supplied, `failed to find tls base`.

**The trap is that "do I use wasm-bindgen" is a question about the dependency
graph, not about your source.** It arrives transitively, and a dev-dependency
counts, because tests and doctests are where threads usually get spawned. One
real case reached it four levels down with nothing in its own manifest naming
wasm-bindgen:

```
your-crate → test_executors → some_executor → js-sys → wasm-bindgen
```

If you want to know which case you are in:

```bash
cargo tree -i wasm-bindgen --target wasm32-unknown-unknown   # add --all-features if features gate it
```

Empty output means the first three suffice. Anything else means all five.

The word to reach for here is **backend**, not "shim". A shim is named for the API
it *provides*; the backend is whatever *implements* it, and only the backend
decides whether the CLI runs. The two shim workspaces sit on opposite sides, and
their directory names point the wrong way:

| workspace | you write | implemented by | backend | exports |
|---|---|---|---|---|
| `backend-wasm-bindgen/` | `wasm_lite` APIs | real wasm-bindgen | **wasm-bindgen** | five |
| `backend-wasm-lite/` | `wasm-bindgen` APIs | real wasm_lite | **wasm_lite** | three |

So "we use the shim" settles nothing — `backend-wasm-lite/` is the one where
wasm-bindgen is *not* the backend. "We are on the wasm-bindgen backend" settles
it, and is the same question `cargo tree -i wasm-bindgen` answers.

Unless you have checked, prefer the full five. They cost two flags, they do no
harm to a non-interop build, and the alternative is a configuration that works
until someone bumps a dependency.

### When it goes wrong

**`TypeError: Cannot set properties of undefined (setting 'value')` at
`wl_worker.js`**, usually reported as `wasm_lite: worker failed to start`. The
module does not export `__stack_pointer`, so the worker bootstrap has nothing to
set the new thread's stack on. Add `-C link-arg=--export=__stack_pointer` — to
**both** lists above. Missing it from `rustdocflags` alone fails every doctest
that spawns a thread while every other test passes, which reads like a threading
race and is not one.

The same shape applies to the other four symbols, so prefer copying the whole
block to adding back one line at a time.

The core `spawn` is **detached** (no `JoinHandle`). The std-like layer with
`spawn -> JoinHandle`, `park`/`unpark`, `Mutex`/`Condvar`/`RwLock`/`mpsc` lives in
`wasm_lite_std` (a port of `wasm_safe_thread`, retargeted off wasm-bindgen onto
this primitive + `core::arch::wasm32` atomics).

Both the **sync** and **async** paths work. Since the main thread can't block,
`wasm_lite_std` ships a small event-loop executor: `spawn_local(future)` runs a
task on the current realm's event loop.

In a stable non-atomic build, its waker schedules another host turn through
`__wl_schedule` (currently a zero-delay timeout). In a shared-memory build, a
pending executor instead sleeps on `Atomics.waitAsync` (`__wl_wait_async`)
rather than polling. Those wakes are edge-triggered and cross-thread: each
executor owns a wake atom, and a task's `Waker` bumps it and issues
`memory.atomic.notify`, resolving the owning realm's `waitAsync` Promise even
when the notify comes from another worker. That is what lets
`JoinHandle::join_async().await`, `Mutex::lock_async().await`, and the other
cross-thread futures remain non-blocking on the main thread.

## Non-blocking timers

`wasm_lite_std::sleep_async(duration)` is safe on the browser main thread in
both wasm modes. Generated glue keeps a cancellable host-timer registry; dropping
the future cancels its timer, and durations beyond JavaScript's signed 32-bit
timeout limit are split into multiple legs. In a shared-memory worker, timer
arm/cancel messages are proxied to the root realm so a timer can outlive the
worker that created its future.

The synchronous `sleep` still blocks. On an atomics worker it uses
`Atomics.wait`; in a stable non-atomic wasm build it can only busy-wait, blocking
that realm's event loop, so browser code should prefer `sleep_async`.

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
  bootstrap polls the exported `__wl_executor_idle()` and only reports completion
  + `close()`s once the executor has drained; its parent then frees the TLS/stack.
  Thus `spawn_local` is correct on *any* thread, not just main (proven in
  `examples/worker-spawn-local-demo`). Residual
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
  deferring it possible. (For a *threaded* async doctest, rustdoc links with
  `rustdocflags`, not `rustflags`, so the crate must repeat the atomics/link args
  under `[build] rustdocflags` — see `examples/async-doctest-demo`. A stable
  single-realm async doctest needs none of those flags.)

`wasm_lite_std` installs a single canonical panic hook (once, on first spawn) that
logs each panic exactly once with thread attribution and routes it to the join
channel — so it owns the panic hook for threaded programs; install any custom hook
(`set_panic_hook`) *before* the first spawn.

### Inspecting live workers and recent console output

Enable `wasm_lite_std`'s opt-in `diagnostics` feature when an in-page debugger
needs state it cannot get from browser APIs:

```toml
[dependencies]
wasm_lite_std = { version = "0.1", features = ["diagnostics"] }
```

`wasm_lite_std::diagnostics::threads()` returns a snapshot of threads spawned
through this crate: a stable id, the optional `Builder` name, spawn time, and
`Spawned`, `Running`, `Finished`, or `Panicked` state. `Spawned` specifically
means the browser accepted the spawn request but the Worker has not entered its
Rust closure yet. Finished threads disappear after their spawn trampoline is
released, so this is a live registry rather than an unbounded history.

The same feature enables the bounded console ring in `wasm_lite`:

```rust
let snapshot = wasm_lite::console::records_since(cursor);
cursor = snapshot.next_cursor;
for record in snapshot.records {
    // Send record.level and record.message to your debugger.
}
if snapshot.dropped != 0 {
    // The reader fell behind the 256-record ring.
}
```

Calls through `wasm_lite::console` are recorded before they reach JavaScript,
including panic messages written by `set_panic_hook`. With the feature off,
neither the registry nor the ring and its spawn/log bookkeeping are compiled.

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
available. A stable non-atomic wasm module has no spawned realms, so it always
reports `true` and `available_parallelism()` reports one.

[`std::time`]: https://doc.rust-lang.org/std/time/
[`web-time`]: https://crates.io/crates/web-time
