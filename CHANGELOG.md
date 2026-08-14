# Changelog

All notable changes to this project will be documented in this file.

## 0.1.1 - unreleased

### Added

- **`#[wasm_lite_test]` now honours `#[should_panic]` and `#[ignore]`.** It used
  to emit the annotated function verbatim and call it, so both attributes rode
  along inert: nothing read them, and rustc does not warn about either on a
  function that isn't a libtest `#[test]`. `#[should_panic]` therefore produced a
  false *failure* — a correct test reported as broken — and `#[ignore]` ran the
  test anyway. Both are the same class of bug as the `async fn` and non-`()`
  return cases the macro already rejected: a result that does not mean what it
  says.

  They are read from the ordinary libtest attributes and deliberately left on
  the function, so the `cfg_attr` pattern this project uses everywhere — a
  native `#[test]` and a wasm32 `#[wasm_lite_test]` on one function — keeps
  honouring a single `#[should_panic]` on both targets. Rejecting them at
  compile time would have broken that.

  `#[ignore]` skips and reports as `ignored`, with `--include-ignored` and
  `--ignored` following libtest. `#[should_panic]` inverts the verdict rather
  than catching anything: the module traps and each case gets a fresh page load,
  so the poisoned instance is discarded either way, and `expected = "…"` matches
  the message the panic hook logged. `#[wasm_lite_bench]` takes `#[ignore]` but
  rejects `#[should_panic]` — a benchmark that panicked measured nothing.

- **`cargo test` works on wasm-bindgen interop modules.** Test mode used to exit
  2 with "does not yet support wasm-bindgen interop modules", while `cargo run`
  and doctests handled them fine. The sharp edge was that enabling wasm_lite's
  own `wasm-bindgen` feature — the `JsValue` bridge — is exactly what puts the
  interop descriptors in the module, so turning on a supported feature silently
  cost you the ability to run your tests, and only after a full build.

  The runner now builds the interop bundle in test mode too. The wasm-bindgen
  CLI preserves `__wasm_lite_tests`, `__wasm_lite_imports`, and the
  `__wl_test_*` exports — it only strips its own schema section and the debug
  sections — so discovery and invocation needed no change. What was missing was
  a loader that exports `instantiate` rather than running on import, since a
  test target is driven one case per page load.

  Threads remain the gap: an interop bundle has no worker bootstrap, so
  `#[wasm_lite_test(worker)]` in an interop module is still unsupported.

- **`wasm_lite_std`'s doctests now run on wasm32 in the gate.** They are the
  only place several threading and async APIs are exercised the way a user
  writes them — `worker_doctest!`, `async_doctest!`, the blocking lock and
  condvar handshakes — and `scripts/wasm32/docs` only ever ran `cargo doc`,
  which builds those examples without running them. A doctest that failed in
  the browser was therefore indistinguishable from a passing build, which is
  how a whole class of threading bug reached users. `scripts/wasm32/tests` now
  runs them under atomics.

### Changed

- **The threaded build needs three linker exports, not five.** Every recipe here
  asked for `__stack_pointer`, `__tls_base`, `__tls_size`, `__tls_align` and
  `__wasm_init_tls`. Only three are ever read — the worker sets
  `__stack_pointer` and calls `__wasm_init_tls`, and the spawning side reads
  `__tls_size` to size the TLS block. `__tls_base` and `__tls_align` appear
  nowhere in the generated JavaScript, and a build that omits them spawns and
  joins threads correctly; they were inherited from wasm-bindgen's recipe and
  carried forward. They are gone from the docs, the example configs, the test
  scripts, and the build error, so nobody copies them again.

  Verified by dropping one flag at a time from a working configuration and
  running a real spawn-and-join in a browser. The same sweep found
  `+bulk-memory` and `+mutable-globals` are not required either — `+atomics`
  alone links and runs — but those are kept in the recipes, since they are
  defaults on current toolchains rather than no-ops, and spelling them out costs
  nothing on an older one.

### Removed

- The `test_executors` dev-dependency of `wasm_lite_std`. It was only ever
  reached from doctests, and `async_doctest!` covers that job on every target.

### Fixed

- **A worker that failed to start hung until the timeout.** The runner detected
  the failure and logged it, then went on waiting for a verdict that could never
  arrive, and finally reported "timed out after 30s … raise
  `WASM_LITE_TIMEOUT_SECS` if it just needs longer" — advice that could not
  possibly help, since nothing was still running. Eight such doctests cost four
  minutes of pure timeout and pointed away from the real cause. A worker that
  never starts will never resolve the join waiting on it, so it is now reported
  as an ending rather than a delay: the glue records the failure where the
  runner can see it, and the run stops immediately with the reason.

- **The default browser lost the output that explained the failure.** On the
  same crash Chrome reported the underlying `TypeError` while Firefox — the
  default — gave `no such window: Browsing context has been discarded`, twice,
  and nothing else. The cause is that a discarded context makes every later
  `eval` fail, so reading the console *after* noticing the failure got nothing;
  Chrome only looked better because it happened not to discard the page. The
  runner now keeps a rolling snapshot of the console as it waits, so whatever
  the page logged survives the page, and reports a discarded context as "the
  page was discarded — it crashed, or ran out of memory" rather than as raw
  WebDriver text.

- **The documented atomics build recipe did not produce a working worker.**
  `wasm_lite_std`'s module docs gave `RUSTFLAGS='-C
  target-feature=+atomics,+bulk-memory'` and nothing else. That enables the
  feature without sharing the memory, so following it exactly produced a module
  whose every spawn failed at runtime with `imported JS function
  __wasm_lite.__wl_spawn threw: __wl_spawn_unavailable`. The recipe now carries
  the link args and points at the full `.cargo/config.toml`.

- **A build that can spawn but has nowhere to spawn into is now a build error.**
  The above failed as a JS exception thrown from an import, which traps the
  instance — taking down a whole test run mid-suite — and arrived with generic
  advice to "bind the import as `Result<_, JsValue>`", which is no help at all
  when the import is wasm_lite's own and you never bound it. Everything needed
  to say so is known when the glue is generated: the module imports `__wl_spawn`
  only if its code can reach `thread::spawn`, and its memory either is shared or
  is not.

  This is a real contradiction rather than a lesser configuration, which is why
  it is an error and not a warning: `Builder::spawn` selects its implementation
  on `#[cfg(target_feature = "atomics")]`, so enabling atomics compiles *out*
  the path that would report `io::ErrorKind::Unsupported` and compiles *in* the
  one that calls `__wl_spawn`. There is no graceful degradation left to
  preserve. A module built without `+atomics` never imports `__wl_spawn` at all,
  so the single-threaded build that does degrade gracefully is never faulted.

- **A threaded build missing a linker export failed unreadably.** The five
  thread symbols the worker bootstrap needs — `__stack_pointer`, `__tls_base`,
  `__tls_size`, `__tls_align`, `__wasm_init_tls` — are exported only on request,
  and `wasm-ld` supplies none of them implicitly, not even in a
  `--shared-memory` build. A link line that omitted one still linked, still
  reported a spawned thread, and then died inside generated JavaScript with
  `TypeError: Cannot set properties of undefined (setting 'value')` at
  `wl_worker.js` — no symbol named, no flag named, no file of yours mentioned.
  The check keys on a *shared* memory import, not just on the presence of
  `__wl_thread_entry`: the core crate keeps that symbol alive whether or not
  anything can spawn, so an ordinary single-threaded module exports it too.
  Without a shared memory the glue emits no `__wl_spawn` at all and `spawn`
  reports `Unsupported`, which is a legitimate build that needs none of these
  flags.

  In a **test** binary it was quieter still: the worker throws before running
  the closure, so nothing ever resolves the join and the test reports as a bare
  30-second timeout — reading as "thread spawning is broken" rather than as a
  bad link line.

  `wasm_lite build`, `wasm_lite run`, and the test/benchmark harness now read
  the module's export section and refuse to generate glue for a module that
  spawns threads without them, listing what is missing and the flags to add.
  The harness path matters most, since a test binary is usually where a project
  spawns its first thread. A module that cannot spawn is never faulted, so
  atomics-without-threads still builds. Nothing can supply these
  automatically: cargo does not propagate `rustc-link-arg` from a dependency's
  build script, and doctests are linked by rustdoc, which build scripts do not
  reach at all.

  The doctest half of this is its own trap, and `docs/threads-and-async.md` now
  carries the whole configuration to copy: rustdoc links doctests itself, does
  not read `rustflags`, and ignores `rustdocflags` under a `cfg(...)` predicate,
  so the section must be keyed by the exact triple. Miss either half and every
  doctest that spawns a thread fails while every other test passes — which reads
  like a threading race and is not one.

- **Sixteen `wasm_lite_std` doctests could not compile for wasm32.** The async
  examples — `lock_async`, `lock_async_timeout`, `with_async`, the `RwLock`
  pair, the `wait_async` family — drove their futures with
  `test_executors::spin_on`, and `test_executors` was a deliberately native-only
  dev-dependency: on wasm32 it pulls wasm-bindgen into the test binary, which
  the runner refuses as an interop module. The `if cfg!(target_arch = "wasm32")
  { return; }` guard those examples carried never helped, because `cfg!` is a
  runtime branch and the name still has to resolve at compile time. They now use
  this crate's own `async_doctest!`, which drives the future on the event-loop
  executor in the browser and blocks on native, so the examples are checked in
  both worlds instead of one. The four `Condvar` examples that spawned a
  notifier thread also stop reaching for an executor inside the spawned closure:
  they use `wasm_lite_std::spawn` and the blocking `lock_block`, which is what a
  spawned thread should do anyway, and the condvar handshake orders the two
  sides without the `sleep` they used to need.

- **Parallel doctests started more browsers than the machine could hold.**
  `cargo test --doc` invokes the runner once per doctest and runs those in
  parallel across every core, and each invocation launched its own browser with
  nothing coordinating them. On a host with more cores than spare gigabytes the
  result was not an out-of-memory message but arbitrary, unrelated tests failing
  with `read: Resource temporarily unavailable (os error 11)` from a thread the
  browser could not spawn, or `no such window: Browsing context has been
  discarded` after a content process was killed — while the same tests passed
  under `--test-threads=1`. The runner now admits a bounded number of browsers
  and makes the rest wait: free memory divided by roughly a gigabyte each,
  clamped to the core count, overridable with `WASM_LITE_MAX_BROWSERS`. The
  The per-browser estimate is deliberately generous (1.5 GiB) and a further
  1.5 GiB is held back for everything that is not a browser: the competing load
  is usually a build, and `cargo` and `rustc` can take gigabytes *after* the
  free-memory reading is taken. The limit is also re-read on every attempt
  rather than sampled once, because a runner can wait minutes for a slot and the
  memory that justified four browsers on arrival may justify one by the time it
  is served. An estimate that admits one browser too many costs a confusing
  failure; one too few costs a little time.

  The limit lives in slot files rather than in process memory, because rustdoc
  spawns a separate runner *process* per doctest and an in-process lock would
  not see the others; a slot whose holder was killed is reclaimed by the next
  waiter.

## 0.1.0 - 2026-08-14

The first release. `wasm_lite` binds JavaScript and Rust to each other on
`wasm32-unknown-unknown` with zero runtime dependencies, moving the work a
runtime would otherwise do into a host-side codegen step that reads descriptors
out of custom wasm sections.

Nothing has shipped before this, so everything below is new to anyone arriving
from crates.io. The **Changed** and **Fixed** sections are kept for the people
who have been building against the repository: they record where behaviour moved
during development, and each one is a decision worth knowing about even if you
never met the bug.

### Added

- `wasm_lite`, a dependency-light Rust/JavaScript binding system for `wasm32-unknown-unknown`.
- Descriptor-based import, export, class, and test metadata emitted into custom wasm sections.
- Host-side `wasm_lite_codegen` for dependency-free descriptor parsing and generated ES module glue.
- `wasm_lite`, one CLI with two subcommands: `build` generates the JavaScript
  glue for a compiled module, and `run` serves it and drives a real browser.
  `run` is what `cargo run`, `cargo test`, and rustdoc doctests go through —
  point the target at it with `runner = ["wasm_lite", "run"]`. A single
  `cargo install wasm_lite_cli` provides both, so nothing needs a checkout.
- Core ABI support for strings, byte slices, vectors, `JsValue` handles, `Option`, `Result`, and sret payloads.
- Proc-macro support for `import!`, `#[export]`, `#[wasm_lite_test]`, and `js_class!`.
- Browser-oriented exports, imports, doctests, test suites, panic reporting, and interop examples.
- Threading, atomics, async execution, worker bootstrap, and `wasm_lite_std` synchronization/time APIs.
- CI, formatting, clippy, docs, and wasm test scripts for release validation.
- `wasm_lite::fetch` — the Fetch API plus the slice of the Streams API a
  response body needs: `fetch`, `origin`, `RequestInit`, `Headers`, `Response`,
  `ReadableStream`, `ReadableStreamDefaultReader`. Every `Headers` method is
  fallible: an invalid header name throws a `TypeError`, and an infallible
  binding would surface that as an unrecoverable wasm trap. Deliberately smaller than the
  `web-sys` surface it replaces: no `Request` (`fetch` takes a URL string), no
  `window()`/`WorkerGlobalScope` downcast (`fetch` and `origin` read off
  `globalThis`, which is both), and chunks arrive as `Vec<u8>` rather than a
  `Uint8Array`. Browser-tested in Firefox and Chrome
  (`crates/wasm_lite_std/tests/fetch.rs`).
- `wasm_lite::websocket` — `WebSocket`, `BinaryType`, `Event`, `MessageEvent`,
  `CloseEvent`. A message's payload comes back as `Vec<u8>` or `String`
  (`MessageEvent::data_bytes`/`data_text`) rather than a `JsValue` to downcast,
  and `onerror` hands back a plain `Event`, which is what browsers actually
  fire. Round-tripped against the runner's echo endpoint in Firefox and Chrome.
- `wasm_lite::dom` — `Window`, `Document`, `Element`, `CssStyleDeclaration`,
  `MouseEvent`, `WheelEvent`, `KeyboardEvent`, and `is_main_thread`. One
  `Element` type rather than web-sys' `Element`/`HtmlElement`/`HtmlCanvasElement`
  split, whose practical effect on calling code was a chain of unchecked casts
  between types with no runtime distinction. `window()` answers `None` on a
  worker instead of requiring a downcast to find out.
- `wasm_lite::event::Event` — the DOM event base type, re-exported from both
  `dom` and `websocket`.
- `JsValue::as_bytes()` / `JsValue::from_bytes()` — read an `ArrayBuffer` or
  `Uint8Array` into a `Vec<u8>`, and copy bytes into an unshared `Uint8Array`.
  The copy is not optional: a borrowed `&[u8]` reaches JS as a view over wasm
  memory that is valid only for the call, and in a `+atomics` build it is a
  *shared* view, which `WebSocket.send` and a `fetch` body both reject.
- `JsValue::to_js_string()` and `Display for JsValue` — JS's own `String(v)`, so
  a rejected promise reports `TypeError: …` instead of a value-table index.
  `Debug` still prints the index. Fallible under the hood with a fallback, since
  this is reached from error paths and `String()` throws for an object with no
  `toString`.
- Benchmarking: `#[wasm_lite_bench]`, `Bencher`, and `bench_main!`, driven by the
  runner one page load per benchmark and reported in `cargo bench`'s format.
  `cargo bench` measures; `cargo test --benches` runs each once without
  measuring. Timing is batch-calibrated because `performance.now()` is coarsened
  to 5 µs even under cross-origin isolation — see
  [docs/testing.md](docs/testing.md#benchmark-in-a-browser).
- `Bencher::iter_custom_async` and `async fn` support in `#[wasm_lite_bench]`,
  for work that cannot be driven synchronously (a `requestAnimationFrame` loop,
  an awaited GPU submission) or whose setup should be excluded from the
  measurement. Fails closed the same way the sync path does.
- `wasm_lite_std::sleep_async` (+ `MAX_TIMEOUT`) — non-blocking sleep, usable on
  the browser main thread where `sleep` traps. Owns its closure, so a dropped
  sleep cancels its timer, and chains sleeps longer than `setTimeout`'s 32-bit
  delay.
- `wasm_lite_std::worker_doctest!` — run a *blocking* doctest on a worker, the
  counterpart to `async_doctest!`. A doctest otherwise runs on the main thread,
  where `join`/`park`/`recv_block` trap.
- `wasm_lite::console::{warn, info, debug, trace}`.
- `wasm_lite::timer` — `setTimeout`/`clearTimeout` and
  `requestAnimationFrame`/`cancelAnimationFrame`, plus
  `wasm_lite_std::request_animation_frame`, which takes an `FnOnce` and keeps it
  alive until the frame fires. `requestAnimationFrame` is main-thread only: a
  worker has no such global and the call throws there.
- `wasm_lite::performance::time_origin()` — `performance.timeOrigin`.
- Runner environment variables, now documented in
  [docs/testing.md](docs/testing.md#configure-the-runner): `WASM_LITE_GPU` (a
  real WebGPU adapter in headless Chrome), `WASM_LITE_BROWSER_ARGS`,
  `WASM_LITE_SERVE_DIR` (serve a directory alongside the program; confined to
  that directory — `..`, absolute components and symlinks pointing outside are
  all refused, and the generated routes always win),
  `WASM_LITE_TIMEOUT_SECS`, and `WASM_LITE_RUN_SECONDS` (watch a long-running
  `bin`).
- The `wasm_lite_std` browser suite now runs in **Chrome as well as Firefox**
  (`scripts/wasm32/tests`), with `a_worker_can_spawn_a_worker` and
  `nesting_composes_to_a_third_level` covering nested worker spawn.
- Bundle-specific artifact names. `wasm_lite_codegen::generate_glue_with_worker`
  and `interop_loader` let a caller point the generated glue at its own sibling
  modules, and the CLI uses them: a shared-memory module now writes
  `<output>.worker.js`, and a wasm-bindgen interop module writes
  `<output>.wasm`, `<output>.wl.js`, and `<output>.wb.js`. Two bundles in one
  directory no longer fight over a single `wl_worker.js`.
- `wasm_lite_codegen::SliceElem` is exported from the crate root. `AbiArg::Slice`
  carried it as a payload, so callers could match the variant but could not name
  its type, call `js_array()`, or reach its derived impls. Each variant now
  documents its JS typed-array counterpart.
- Rounder trait coverage on public types: `Debug` for `Bencher` and `SleepAsync`,
  `Hash` for `DeltaMode`, and `Hash`/`Default`/`#[non_exhaustive]` for
  `BinaryType`.
- Every module carries a `//!` header. `-D missing_docs` does not reach private
  modules, so twenty of them had gone undocumented without the docs gate
  noticing.

### Changed

- **Ownership transfers across the ABI are `unsafe`.** `JsValue::__wl_from_abi`,
  `__wl_free`, `__wl_thread_free`, `__wl_thread_entry`, and the closure
  trampolines are `#[doc(hidden)]` but reachable, and each one takes ownership of
  something it cannot validate — two owned handles for one table slot corrupt the
  host free list when both drop. They are now `unsafe` with the precondition
  written down.
- **`#[export]` rejects what it cannot lower.** Generic parameters (a lifetime
  generic could smuggle in a `'static` bound and defeat the dangling-reference
  check), `unsafe fn` (the generated entry point is callable by arbitrary
  JavaScript and is intentionally safe), `&'static` borrows hidden inside an
  `Option`, `&mut` borrows of the temporary argument buffer (the glue has no way
  to copy mutations back), and numeric ABIs the glue does not implement. Each is
  a compile error naming the offending parameter rather than glue that reads the
  wrong bytes.
- `import!`'s `#[variadic]` is accepted only on functions, methods, and
  constructors — the shapes that can actually spread arguments — and the
  descriptor parser enforces the same rule on the host side.
- **Generated JS treats descriptor text as data, never source.** Import objects
  are built with `Object.create(null)`, so a binding named `__proto__` or
  `constructor` defines that import instead of mutating `Object.prototype`;
  exports are emitted as a stable local identifier plus a quoted alias; and the
  string escaper covers `U+2028`/`U+2029` and every other control character.
- The descriptor and export parsers reject malformed lines instead of filling in
  defaults. A truncated line, an extra field, an unknown variadic flag, or an
  empty tag between two commas is now an error naming the line — previously
  several of these produced glue that quietly read the wrong argument.
- Async timeouts no longer spawn a sleeping thread. `lock_async_timeout` and
  `Condvar`'s async waits use the cancellable timer service, which is what makes
  them work on a stable non-atomic build, and a dropped waiter now unregisters
  itself from the queue instead of being woken forever.
- `WASM_LITE_RUN_SECONDS` now *selects* headless mode, so it works with
  `cargo run`. It is documented as "watch a long-running bin for N seconds", but
  it is only read on the headless path, and `cargo run` took the interactive one
  — which serves forever and prints nothing.
- A headless run that fails now prints the reason the program recorded
  (`globalThis.__wl_done.error`). It previously printed the word `FAILED` and
  nothing else, under a log that can be tens of thousands of lines of a program
  working normally.
- The runner serves a WebSocket **echo endpoint** at `/__wl_echo`, so bindings
  to an API that only means anything against a peer can be tested against one.
  Deliberately minimal: no extensions, no subprotocol negotiation.
- The runner's HTTP server honours the request method and the `Range` header. A
  `HEAD` now returns headers without a body; `Range: bytes=a-b` returns 206 with
  `Content-Range`, and an unsatisfiable range returns 416. It previously answered
  every request with a full 200, which made a range-reading client's primary path
  untestable. Suffix (`bytes=-500`) and multi-range requests are refused rather
  than mis-answered, and a *backwards* one (`bytes=10-5`) gets a 416 rather than
  panicking the connection thread on a reversed slice.
- The CLI writes multi-file bundles through sibling temporary files, refuses two
  artifacts that would land on the same path, and will not replace an implicit
  worker artifact unless it carries the generated-file marker. A failed run no
  longer leaves half a bundle behind, and a broken pipe on stdout is not an
  error.
- Moved macro parsing onto a unified `syn`/`quote` build-time implementation
  while keeping runtime crates dependency-free, and moved the proc-macro crates
  to `syn` 3. `syn`/`quote` cost zero bytes in the `.wasm`.
- Improved documentation for the binding model, testing flow, threading/async
  behavior, interop, and migration story.

### Fixed

- **`&mut` slice arguments lost their mutable provenance.** `import!` passed
  `as_ptr()` — an immutable reborrow — for `&mut [u8]`, `&mut [T]`, and
  `Option<&mut [u8]>`, and JS then wrote through a pointer derived from a shared
  reference. The comment claiming this was fine ("the pointer is only a base
  address") was wrong about provenance, not about addresses. Now `as_mut_ptr()`,
  with a compile-checked expansion test for each shape.
- **sret payloads were written at the wrong width.** `#[export]` wrote every
  non-`i32`/`u32` scalar as an `f64`, which disagreed with the exact `DataView`
  width the glue reads back and failed to compile outright for most types. An
  explicitly written `-> ()` is now accepted as the same ABI as an omitted
  return.
- **`Instant` was not comparable across threads.** It was raw
  `performance.now()`, which is per-realm — a Web Worker's zero is the moment
  that worker started, so an `Instant` taken on one thread and read on another
  was out by however long the page had been up. `Instant` is `Ord` and deadlines
  cross threads constantly (`join_async`, every `*_timeout`, an executor's
  `poll_after`), so this affected anything threaded. Now
  `performance.timeOrigin + performance.now()`.
- **A Web Worker could not spawn a Web Worker in Chrome.** The spawn returned a
  handle, nothing reported an error, and the thread never ran; joining it blocked
  forever. Chrome fetches a nested worker's module script through its parent, and
  a parent in `Atomics.wait` — which is every blocking primitive — never services
  it. Worker *creation* is now delegated to the main thread. Firefox was
  unaffected, which is why a Firefox-only CI stayed green.
- A spawned worker that fails to start now reports through `onerror` /
  `onmessageerror` instead of failing silently, and a worker bootstrap that
  throws before the closure runs reports through `error` / `unhandledrejection`
  rather than becoming an invisible unhandled rejection.
- A worker's stack and TLS are freed by the parent. The worker cannot free the
  stack it is standing on, so those allocations leaked on every spawn.
  `__wl_thread_alloc` also aborts on allocation failure rather than returning
  null, which the glue would have used as address zero and corrupted linear
  memory with.
- `js_class!` used the raw identifier as the default JavaScript property name, so
  `fn r#type(&self)` looked for a property literally called `r#type`. The raw
  prefix is an escape hatch for the Rust parser; the JS property is `type`.
- Only the first `__wl_exports` custom section was read. A module that emitted
  more than one silently lost every export in the others.
- Member-path generation was quadratic in the number of dotted segments, since
  each one reformatted the whole accumulated path.
- **A killed runner orphaned a spinning browser.** The WebDriver session is
  closed from `Drop`, which a signal never runs, so a killed runner left
  geckodriver and a headless browser alive with the test page still executing —
  and a page that was spin-waiting pinned a core until someone noticed.
  SIGINT/SIGTERM/SIGHUP are now handled by a watchdog thread that closes the
  session against a 5 s deadline and exits `128 + signal`, and the session is
  registered as soon as the driver spawns rather than once it is ready. SIGKILL
  cannot be handled, so the page defends itself: every WebDriver script stamps a
  heartbeat and the test shell discards itself after 30 s without one.
- **A test binary could be misread as a `cargo run` and served forever.** Cargo
  invokes the runner identically for both, so the artifact path is the only
  signal, and only the `deps/` layout was recognised. Cargo 1.99-nightly emitted
  a binary under `…/build/<pkg>/<hash>/out/`, which took the interactive path and
  hung CI rather than failing it. Both layouts are recognised now, and
  `open_browser` reports a failed launch instead of treating a non-zero exit as
  success and waiting for a viewer that could not arrive.
- A timeout now prints the console the program captured before it hung, on all
  three run shapes (`bin`, test suite, bench suite). Previously it printed only
  "timed out".
- Three held-lock timeout tests raced: a holder that kept the lock for a fixed
  duration while the other side asserted a short attempt failed. They now hold
  until told to release.
- Crate-level docs had drifted: both logo URLs pointed at a nonexistent
  `art/logo.png` (and `wasm_lite_std`'s at a nonexistent repository), and
  `wasm_lite`'s workspace table, documentation table, and runner prerequisites
  were behind the README. Three files in the `shims_wasm_bindgen` workspace were
  also missing their SPDX headers.
