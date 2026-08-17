// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixtures that **must fail**, driven by `scripts/wasm32/negative`.
//!
//! Every other suite in this repo checks that passing code passes. These check
//! the harder direction: that failing code is *reported* as failing. Nothing
//! else can catch a runner bug that swallows a failure, because a swallowed
//! failure looks exactly like a green run — which is how both of the bugs
//! covered here survived so long.
//!
//! Run them with `scripts/wasm32/negative`, never with a bare `cargo test`: a
//! plain run of this crate is *supposed* to come back red.
//!
//! The doctests below cover the async-verdict half; `tests/mixed_harness.rs`
//! covers the libtest-entry-point half.

/// A fast `async_doctest!` that passes.
///
/// It exists only to finish first. `async_doctest!` spawns its body and returns
/// immediately, so in an edition-2024 merged doctest bundle both of these are
/// outstanding at the same time against one instance.
///
/// ```
/// wasm_lite_std::async_doctest!(async {});
/// ```
pub fn fast() {}

/// A slow `async_doctest!` that MUST fail.
///
/// When the page's verdict was a single slot rather than a count of outstanding
/// bodies, [`fast`] published "passed" for the whole page, the runner exited on
/// it, and this panic was never seen. The sleep is what orders the two; the
/// failure itself is immediate once it is reached.
///
/// The fixture greps for the panic *message*, which pins down two things at
/// once. The message can only appear if this body actually ran (so the page's
/// verdict was not published early on [`fast`]'s behalf), and it can only reach
/// the console if a panic hook was in force when the body panicked — which in a
/// merged bundle it is only because `async_doctest!` installs one itself. A
/// `set_panic_hook()` call here in `main` would not survive: libtest restores
/// the pre-test hook the moment a doctest's `main` returns.
///
/// Note there is deliberately no `set_panic_hook()` call below. Its absence is
/// the point — the macro has to hold up the documented promise on its own.
///
/// ```
/// wasm_lite_std::async_doctest!(async {
///     wasm_lite_std::sleep_async(std::time::Duration::from_millis(200)).await;
///     panic!("SLOW DOCTEST FAILED");
/// });
/// ```
pub fn slow() {}
