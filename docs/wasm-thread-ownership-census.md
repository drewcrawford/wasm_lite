# Census: thread ownership in the wasm-bindgen ecosystem

*(Supporting data for the [roadmap](./roadmap.md)'s interop items — specifically why
the fake-wasm-bindgen `[patch]` shim can declare thread-owning crates out of scope
rather than impersonate `wasm-bindgen-rayon`/`web_sys::Worker`.)*

**Question.** If a wasm_lite-hosted app absorbs wasm-bindgen crates (e.g. wgpu) via
a `[patch]` shim, how many of those crates *own threads* — i.e. spawn Web Workers
themselves, which the shim would otherwise have to reimplement on wasm_lite's worker
substrate?

**Source.** crates.io db-dump, snapshot **2026-06-29-020013** (290,965 crates;
283,170 with a non-yanked version; 29,316,138 dependency rows). Edge model: each
crate's **latest non-yanked version**, **normal** deps only (kind=0; build/dev
excluded). "Ecosystem" = crates with a direct dep on `wasm-bindgen`, `js-sys`, or
`web-sys`. Reproduce with `analyze.py` / `owners.py` against the dump's
`crates.csv` / `versions.csv` / `dependencies.csv`.

## Headline numbers

wasm-bindgen ecosystem size: **5,063 crates.**

| signal | count | share |
|---|---|---|
| **Direct thread owners** (`wasm_thread` / `wasm-bindgen-rayon` / `wasm_safe_thread`) | **49** | **0.97%** |
| …including `rayon` as an "owner" | 342 | 6.75% |
| Thread-owner anywhere in tree (transitive, excl. `rayon`) | 68 | 1.34% |
| …transitive incl. `rayon` | 5,063 | 100% |
| Requests web-sys `Worker*` features | 193 | 3.81% |

Ecosystem-wide reverse-dep totals for the dedicated wasm-threading crates:
`wasm-bindgen-rayon` **39**, `wasm_safe_thread` **10**, `wasm_thread` **8**.

**Reading it.** Real wasm thread-ownership is **~1%**, concentrated in
parallel-compute crates. The 100% transitive-with-`rayon` figure is an artifact:
`rayon` is in nearly every dep tree but on `wasm32` sits behind
`cfg(not(target_arch = "wasm32"))` (native parallelism that spawns nothing in the
browser) — which is why the census isolates the dedicated wasm-thread crates from
`rayon`. The `Worker*`-feature figure (3.8%) is the honest upper bound on "touches
Workers at all", and it *includes* run-*inside*-a-worker cases (OffscreenCanvas,
like wgpu) that spawn nothing. True spawn-ownership sits between ~1% and ~4%.

## The 49 direct owners

**`wasm-bindgen-rayon`** (37) — overwhelmingly ZK-proof / cryptography and
heavy-math / compute, the classic "opt into SharedArrayBuffer parallelism" niche:

> appcui, autoeq-roomsim, cera-wasm, cvkg-render-gpu, dioprism-resume, filmr_app,
> geo-polygonize-wasm, halo2-wasm, hydroxide, ironshield-wasm, ligerito, math-bem,
> math-solvers, miden-client-web, mopro-ffi, nectar-primitives, nova-scotia,
> pdfcrop, qukit, rammap-core, rammap-core-temp, rgskin, ries, rumoca,
> ruvector-math-wasm, shapezlib, sorex, symbolic_regression_wasm, tfhe, unbinder,
> vec64, voronoid, vorothree, wasm-workers, websnark-rs, whisper-apr, zync-core

**`wasm_thread`** (5) — mostly `gpui`-on-web experiments:

> gpui-web-gpui-unofficial, open-gpui-web, psychophysics, rezcraft, rgpui-web

**`wasm_safe_thread`** (7) — **drewcrawford's own crates** (already in scope for
migration to `wasm_lite_std`, which is the port of `wasm_safe_thread`):

> app_window, exfiltrate, kithara-platform, logwise, some_executor,
> some_global_executor, wasm_safe_mutex

## Takeaway

Excluding your own `wasm_safe_thread` crates (which migrate anyway), the *external*
thread-owning tail is ~42 crates, almost entirely **`wasm-bindgen-rayon`**
ZK/compute. So:

* The fake-wasm-bindgen shim **does not** build `wasm-bindgen-rayon` /
  `web_sys::Worker`-spawn impersonation. Thread-owning crates are **out of scope**;
  an app that needs one hosts on **wasm-bindgen** via the dual shim instead (where
  its threading is native to the substrate).
* If that tail ever becomes worth covering, **`wasm-bindgen-rayon` is the single
  lever** — supporting just it would absorb the large majority of the external
  owners. But it's a niche (in-browser ZK proving / numerical compute), so it stays
  unscheduled.

## Caveats

* This counts **dependency edges**, not actual thread *spawning*; a crate could
  spawn `web_sys::Worker` directly without any of these crates (the 3.8%
  `Worker*`-feature figure is the proxy/upper bound for that).
* Latest-version snapshot; yanked versions excluded; normal deps only.
* No per-edge `target` cfg filtering except the deliberate `rayon` separation; the
  dedicated wasm-thread crates are wasm-only by nature, so their edges are real
  wasm intent.
