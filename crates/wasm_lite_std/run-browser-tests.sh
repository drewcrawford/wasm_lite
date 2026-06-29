#!/usr/bin/env bash
# Run wasm_lite_std's browser test suite (tests/browser.rs) on wasm32 via the
# wasm_lite runner. Requires nightly (atomics ⇒ recompile std) + a WebDriver
# browser (Firefox/geckodriver or Chrome/chromedriver). Pass extra args through,
# e.g. `--no-run` to just build.
set -euo pipefail
cd "$(dirname "$0")/../.."   # workspace root

cargo build -p runner

RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
 -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 -C link-arg=--import-memory \
 -C link-arg=--export=__stack_pointer -C link-arg=--export=__tls_base -C link-arg=--export=__tls_size \
 -C link-arg=--export=__tls_align -C link-arg=--export=__wasm_init_tls" \
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$PWD/target/debug/runner" \
cargo +nightly test -p wasm_lite_std --test browser \
  --target wasm32-unknown-unknown -Z build-std=std,panic_abort "$@"
