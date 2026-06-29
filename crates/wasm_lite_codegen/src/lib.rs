//! Host-side code generation for wasm_lite.
//!
//! The wasm-side `import!` macro records a descriptor for every imported JS
//! function into the `__wasm_lite_imports` custom section. This crate reads that
//! section out of a compiled module and generates the matching JavaScript glue:
//! one shim per import that unmarshals the wasm-level arguments (e.g. a `&str`
//! arrives as a `(ptr, len)` pair to decode from linear memory) and calls the
//! real JS function.
//!
//! It is dependency-free: a minimal wasm binary reader plus a small text parser.

mod descriptor;
mod exports;
mod generate;
mod interop;
mod wasm;

pub use descriptor::{AbiArg, Descriptor};
pub use exports::{Export, ExportRet, exports_from_wasm};
pub use generate::{generate_glue, generate_worker};
pub use interop::{InteropBundle, build_interop, patch_wasm_bindgen_glue};
pub use wasm::{MemoryImport, imported_memory};

/// Name of the custom section the `import!` macro writes descriptors into.
pub const SECTION_NAME: &str = "__wasm_lite_imports";

/// Returns true if the module was produced with wasm-bindgen (carries its
/// schema section), meaning it needs the wasm-bindgen CLI before it can run.
pub fn uses_wasm_bindgen(wasm: &[u8]) -> bool {
    matches!(
        wasm::custom_section(wasm, "__wasm_bindgen_unstable"),
        Ok(Some(_))
    )
}

/// Names of the tests declared via `wasm_lite::tests!`, in order.
///
/// Empty if the module has no test section (i.e. it isn't a test harness).
pub fn test_names(wasm: &[u8]) -> Vec<String> {
    match wasm::custom_section(wasm, "__wasm_lite_tests") {
        Ok(Some(bytes)) => std::str::from_utf8(bytes)
            .unwrap_or("")
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Read import descriptors from a compiled wasm module.
///
/// Returns an empty vector if the module has no descriptor section (e.g. it was
/// built without any `import!` declarations).
pub fn descriptors_from_wasm(wasm: &[u8]) -> Result<Vec<Descriptor>, String> {
    match wasm::custom_section(wasm, SECTION_NAME)? {
        Some(bytes) => descriptor::parse(bytes),
        None => Ok(Vec::new()),
    }
}
