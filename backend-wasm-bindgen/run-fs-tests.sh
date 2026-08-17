#!/usr/bin/env bash
# Exercise wasm_lite_std::fs through the real wasm-bindgen backend.
set -euo pipefail
cd "$(dirname "$0")/.."

GECKO="$(command -v geckodriver)"
CHROME="$(command -v chromedriver)"
RUNNER="$(command -v wasm-bindgen-test-runner)"

cargo test --manifest-path backend-wasm-bindgen/Cargo.toml \
    -p wasm_lite_std --features fs --lib

for DRIVER in firefox chrome; do
    if [[ "$DRIVER" == firefox ]]; then
        env -u CHROMEDRIVER \
            GECKODRIVER="$GECKO" \
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$RUNNER" \
            cargo test --manifest-path backend-wasm-bindgen/Cargo.toml \
                -p wasm_lite_std --features fs --target wasm32-unknown-unknown --test fs
    else
        env -u GECKODRIVER \
            CHROMEDRIVER="$CHROME" \
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$RUNNER" \
            cargo test --manifest-path backend-wasm-bindgen/Cargo.toml \
                -p wasm_lite_std --features fs --target wasm32-unknown-unknown --test fs
    fi
done

# The atomics build adds the cross-realm future handoff test. These are the
# canonical wasm_lite shared-memory flags; wasm-bindgen's transform also needs
# the TLS exports even though the Rust fs code does not read them directly.
source scripts/wasm32/_env
for DRIVER in firefox chrome; do
    if [[ "$DRIVER" == firefox ]]; then
        env -u CHROMEDRIVER \
            GECKODRIVER="$GECKO" \
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$RUNNER" \
            RUSTFLAGS="$WASM32_ATOMICS_RUSTFLAGS" \
            cargo +nightly test --manifest-path backend-wasm-bindgen/Cargo.toml \
                -p wasm_lite_std --features fs --target wasm32-unknown-unknown \
                -Z build-std=std,panic_abort --test fs
    else
        env -u GECKODRIVER \
            CHROMEDRIVER="$CHROME" \
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER="$RUNNER" \
            RUSTFLAGS="$WASM32_ATOMICS_RUSTFLAGS" \
            cargo +nightly test --manifest-path backend-wasm-bindgen/Cargo.toml \
                -p wasm_lite_std --features fs --target wasm32-unknown-unknown \
                -Z build-std=std,panic_abort --test fs
    fi
done
