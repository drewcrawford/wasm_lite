# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- **`#[wasm_lite_test]` registers a libtest `#[test]` off wasm32.** One attribute
  is now the whole story: `cargo test` runs the suite on the host and
  `cargo test --target wasm32-unknown-unknown` runs the same file in a browser,
  with the same names, verdicts and skips. The
  `#[cfg_attr(not(target_arch = "wasm32"), test)]` pairing the docs used to teach
  is no longer needed — though it still works, since off wasm32 the `cfg_attr`
  holding the macro is false and it never runs, so no existing suite has to be
  rewritten.

  A *literal* `#[test]` alongside is worth removing, though. Below the attribute
  it is detected and no second registration is made; above it, rustc expands and
  consumes it before this macro runs, so the case registers twice.

  `examples/dual-demo` is one file run both ways, by `scripts/native/tests` and
  `scripts/wasm32/tests`.

  The one behaviour change to watch for: a test that is meaningful *only* in a
  browser will now be run on the host too. Gate the item with
  `#[cfg(target_arch = "wasm32")]` rather than gating the attribute. Inside a
  `harness = false` target the question does not arise, since libtest is not
  linked there and the registration is inert.

- **`#[wasm_lite_test]` accepts an `async fn`, and drives it.** Previously a hard
  error, on the grounds that the generated entry point would build the future and
  drop it unpolled — a test that could never fail. Now the future is driven:
  `wasm_lite_std::block_on` off wasm32, and on the event loop with a deferred
  verdict in the browser, exactly as `async_doctest!` does. A body that panics
  after an await, hangs, or is dropped therefore fails.

  Since the guarantee is no longer secured by the feature's absence, it is
  secured by fixtures instead: `examples/must-fail-demo/tests/async_body.rs` and
  `async_hang.rs`, driven by `scripts/wasm32/negative`. If either starts passing,
  that is the regression — not a fixture to update.

  `#[should_panic]` and `#[ignore]` move onto the generated libtest test rather
  than staying on the body, which is not itself the test; leaving them there
  would run an `#[ignore]`d case anyway. `(worker)` combines with `async fn` for
  a body that awaits *and* blocks: `block_on` drives it on the worker, while the
  main thread awaits that worker's join. A body returning a value is still
  rejected — it would be discarded, so an `Err` would pass silently.

- **`wasm_lite_std::block_on`.** A supported spelling of what the crate already
  had twice internally, as spin-poll loops behind `__rt::block_on` and
  `test_executor::spawn`. Off wasm32 it now parks on a condvar between polls
  rather than spinning, which matters once `cargo test` is running several at
  once; on wasm32 it blocks, so like `park`, `lock_block` and `recv_block` it
  must not be called on the browser main thread.

- **`std_crate = <path>` on `#[wasm_lite_test]` and `#[wasm_lite_bench]`.** Both
  macros already took `crate = <path>` so a wrapper crate could point the
  generated code at its own re-export of `wasm_lite`. Two expansions also name
  `wasm_lite_std` — the `(worker)` test form and an `async fn` benchmark — and
  those paths were absolute. A wrapper could therefore forward everything except
  the two things that most needed forwarding, and its users got `E0433: cannot
  find wasm_lite_std in the list of imported crates`, pointing at the wrapper's
  macro rather than at anything they wrote.

  It is a second argument rather than something derived from `crate =`, because
  a wrapper is free to re-export the two runtimes under names with no
  relationship to each other. `worker_doctest!` and `async_doctest!` never needed
  it: they are `macro_rules!` and resolve through `$crate` already.

  `examples/reexport-demo` is the regression fixture. It renames both
  dependencies in its `Cargo.toml`, so neither crate name is in its extern
  prelude and any absolute path that creeps back into an expansion becomes a
  compile error here rather than a downstream user's problem.

### Fixed

- **A single `#[wasm_lite_test]` no longer silently disables every libtest test
  in the same binary.** The runner treated "has a registered test section" and
  "has a `main` worth running" as mutually exclusive, so one `#[wasm_lite_test]`
  anywhere in a binary meant `main` was never called — and outside a
  `harness = false` target, `main` is libtest's entry point. Every plain
  `#[test]` beside it, and every other doctest in an edition-2024 merged bundle,
  therefore did not run. Nothing was reported as skipped; the tests simply were
  not in the output and the suite reported `ok`. One crate's doctests went from
  20 passing natively to 1 on wasm32 without a word.

  A harness run now drives the registered suite and then runs `main` as one more
  case, named `main`, which takes part in name filtering and `--list` like any
  other. The exception is positive evidence, not a guess: `test_main!` and
  `bench_main!` stamp a `__wl_noop_main` custom section saying their `main` is
  the `fn main() {}` the linker asked for, and the runner skips it. Absence of
  the marker has to keep meaning "run it" — the module cannot otherwise be told
  apart from a libtest binary, and guessing the other way is what lost the tests.
  Suites using `test_main!` are unchanged, down to their reported counts.

  The recovered verdict is coarse: on `wasm32-unknown-unknown` libtest's own
  output goes nowhere and `panic = abort` stops at the first failure, so a
  failure says *that* something under `main` failed, with the panic message but
  no test name. Use the `cfg_attr` pairing if you want per-test reporting. Coarse
  and visible beats not running them at all.

- **One async doctest finishing no longer passes the whole page.**
  `__wl_async_pending` was a flag and `__wl_test_pass` published
  `__wl_done = { ok: true }` unconditionally, which holds only while a page has
  exactly one deferred body. An edition-2024 merged doctest bundle runs every
  doctest in the crate against a single instance, so the first `async_doctest!`
  to complete declared the page passed, the runner exited on it, and a sibling
  that panicked afterwards was never seen.

  The pair is now a count of outstanding bodies, and the verdict is published
  only when the last one retires. That keeps the fail-closed property the macro
  exists for: a body that is dropped or hangs leaves the count above zero, so the
  page never reports done and the runner's timeout fires, rather than an
  unrelated sibling passing on its behalf. Every reader already tested the value
  in boolean context, where `0` and `undefined` are alike falsy, so nothing else
  had to change.

- **A panic in a deferred doctest body reports its message again.** Calling
  `wasm_lite::set_panic_hook()` at the top of a doctest is the documented way to
  get a panic message instead of a bare `unreachable` trap, and it quietly did
  nothing for `async_doctest!` and `worker_doctest!`. libtest takes the current
  panic hook before each doctest in a merged bundle and restores it when the test
  returns, so a hook installed while `main` ran was gone by the time the event
  loop polled the body — the one moment it was needed. Failures came back as a
  wasm stack trace with no message.

  Both macros now install the hook inside the deferred body, where it is in force
  when the body can actually panic, so their documented promise holds without the
  caller doing anything. A doctest that defers work by some other route still
  needs to install the hook in the deferred part.

- **`wasm_lite_std`'s own unit tests run in a browser.** Sixty-nine of them now
  do, against thirty-four before: the rest were plain `#[test]`s that the
  entry-point bug above had been hiding for as long as it existed. Three had
  never been correct, which nothing could have noticed while they did not run —
  a `cfg_attr(target_arch = "wasm32", should_panic)` on `test_spawn_and_join`
  that is simply wrong on a worker, a `test_is_main_thread` asserting two things
  that cannot both hold on one thread, and an `rwlock` test reaching
  `std::sync::mpsc::recv_timeout`, which panics with "time not implemented on
  this platform" on wasm32. All three are fixed and the suite passes in Firefox
  and Chrome. The six tests that were already explicitly
  `#[cfg(not(target_arch = "wasm32"))]` are left alone.

- **`scripts/wasm32/tests` now opens with a stage of fixtures that must fail.**
  Two of the bugs above shipped because a swallowed failure is indistinguishable
  from a green run, and no passing test can catch that. `scripts/wasm32/negative`
  runs `examples/must-fail-demo` and requires each fixture to fail *and* to name
  the reason, so a fixture that fails because no browser was available does not
  count as covered. A negative fixture that starts passing is a runner
  regression; fix the runner, not the fixture.

- **`wasm_lite_std` no longer breaks a nightly host build under `-D warnings`.**
  The `internal_output_capture` feature was gated on `nightly_rustc` alone, but
  its only caller — the `set_output_capture` call that routes `println!` to the
  browser console — lives in `mod wasm`, which is `cfg(target_arch = "wasm32")`.
  On a nightly build for any other target the feature was therefore declared and
  never used, which `unused_features` reports and `-D warnings` promotes to a
  hard error. Merely depending on `wasm_lite_std` was enough to hit it: a
  downstream crate did, from a script that builds a proc-macro crate — which
  compiles for the host — with the warning-strict flags. The gate now also
  requires `target_arch = "wasm32"`, matching the code it exists for. No
  behaviour change on wasm32.

## 0.1.1

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

- **The shim workspaces are named for their backend.** `shims/` is now
  `backend-wasm-bindgen/` and `shims_wasm_bindgen/` is now `backend-wasm-lite/`.
  The old names described the API each one *provides*, which is the opposite of
  what callers need to know: `shims_wasm_bindgen/` was the one where wasm-bindgen
  is **not** the backend. Since the backend is what decides whether the
  `wasm-bindgen` CLI runs — and so which linker exports a threaded build needs —
  naming them for the API actively pointed the wrong way.

  Nothing in either workspace is published, and nothing can be: both reuse the
  exact package names of the crates they substitute, because `[patch]` matches on
  package name. They are consumed by patching to a path or git, so this renames
  the paths in a `[patch.crates-io]` block rather than breaking a release.


- **The thread-export check now asks for what your backend actually needs.** It
  demanded all five symbols of every threaded build. Three are read by the
  generated glue; the other two exist for the `wasm-bindgen` CLI's threading
  transform, so a module with no wasm-bindgen anywhere in its graph was being
  told to add flags it had no use for. The check reads the module — the same
  schema section that decides whether the CLI runs at all — and requires three or
  five accordingly, saying which case it found and why. A crate that later
  acquires a wasm-bindgen dependency (they arrive transitively, and a
  dev-dependency counts) is told at its next build rather than silently working
  until it doesn't.

  Both checks also moved ahead of the interop branch, so they now apply to
  interop modules too. That is the case they help most: the CLI's own diagnostic
  is `failed to find __tls_align`, which names one symbol, not the flag, the file,
  or the second symbol it will ask for once the first is supplied.

  The vocabulary is now **backend** rather than "shim" wherever it decides
  behaviour, because the shim directories point the wrong way on their own: a
  shim is named for the API it *provides*, while the backend is what *implements*
  it. `backend-wasm-bindgen/` puts real wasm-bindgen under your wasm_lite code (wasm-bindgen
  backend, five exports); `backend-wasm-lite/` puts wasm_lite under your
  wasm-bindgen code (wasm_lite backend, three).

- **The threaded build's five linker exports are documented rather than
  guessed at.** The recipes ask for `__stack_pointer`, `__tls_base`,
  `__tls_size`, `__tls_align` and `__wasm_init_tls`, and it was not written down
  anywhere why. It is now: the generated glue reads three of them — the worker
  sets `__stack_pointer` and calls `__wasm_init_tls`, and the spawning side
  reads `__tls_size` to size the TLS block — while `__tls_base` and
  `__tls_align` are read by nothing here but are required by the **wasm-bindgen
  CLI's** threading transform, which otherwise stops with `failed to find
  __tls_align`.

  All five are required unconditionally rather than the last two only for
  interop, because a crate cannot tell from its own manifest which path it is
  on: a wasm-bindgen dependency arrives transitively, and a test helper pulling
  `wasm-bindgen-futures` is enough. A recipe that works until someone adds a
  dependency is worse than two extra flags.

  A drop-one-flag-at-a-time sweep also found `+bulk-memory` and
  `+mutable-globals` are not required — `+atomics` alone links and runs — but
  those are kept, since they are defaults on current toolchains rather than
  no-ops and cost nothing to name on an older one.

### Removed

- The `test_executors` dev-dependency of `wasm_lite_std`. It was only ever
  reached from doctests, and `async_doctest!` covers that job on every target.

### Fixed

- **A failed interop build leaked its scratch directory.** `build_interop`
  removed the wasm-bindgen CLI's output directory at the end of the successful
  path, but the CLI invocation and its error returns sit between creating that
  directory and reading the output — so a missing CLI, a version mismatch, or a
  module the CLI rejects each left one behind. The directories are empty, so the
  cost is entries rather than disk, but they accumulated unboundedly and did so
  precisely when the tool was already failing. Cleanup is now a destructor, which
  covers every exit path. The name also gained a per-call counter: the pid alone
  meant two `build_interop` calls in one process would share a path and delete
  each other's in-progress output.

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
  were behind the README. Three files in the `backend-wasm-lite` workspace were
  also missing their SPDX headers.
