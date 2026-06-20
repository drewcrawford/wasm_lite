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
mod generate;
mod wasm;

pub use descriptor::{AbiArg, Descriptor};
pub use generate::generate_glue;

/// Name of the custom section the `import!` macro writes descriptors into.
pub const SECTION_NAME: &str = "__wasm_lite_imports";

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
