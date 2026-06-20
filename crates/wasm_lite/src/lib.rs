//! wasm_lite: minimal JavaScript bindings for Rust compiled to wasm.
//!
//! For now this crate hand-writes a single binding — `console.log` — to nail
//! down the layering between Rust, the wasm import boundary, and the JS host.
//! Generalizing this (a macro + codegen for arbitrary imports/exports) comes
//! later.

pub mod console;
