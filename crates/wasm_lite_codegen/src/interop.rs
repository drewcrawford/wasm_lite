//! Interop bundling for modules that mix wasm_lite and wasm-bindgen.
//!
//! Runs the wasm-bindgen CLI to finalize the module and emit its glue, then
//! assembles a loader that merges both glues' import objects and instantiates
//! once. Loader paths are relative, so the bundle works both served at a URL
//! root (the dev runner) and as flat files in one directory (deployment).

use crate::{descriptors_from_wasm, generate_glue};
use std::path::Path;
use std::process::Command;

/// The assembled JS + wasm pieces of an interop bundle.
pub struct InteropBundle {
    /// The wasm-bindgen-finalized module (serve/write as `program.wasm`).
    pub wasm: Vec<u8>,
    /// The entry module that merges both glues (serve/write as `program.js`).
    pub loader_js: String,
    /// The wasm_lite glue (serve/write as `wl_glue.js`).
    pub wl_glue_js: String,
    /// The patched wasm-bindgen glue (serve/write as `wb_glue.js`).
    pub wb_glue_js: String,
}

const LOADER_JS: &str = r#"import { makeImports, setInstance } from "./wl_glue.js";
import { __wbg_get_imports, __wbg_finalize_init } from "./wb_glue.js";

// Neutral handoff slot for the wasm_lite <-> wasm-bindgen JsValue bridge.
globalThis.__wlbridge = {
    _v: undefined,
    put(o) { this._v = o; },
    take() { const v = this._v; this._v = undefined; return v; },
};
globalThis.__wlbridge_put_wb = (o) => { globalThis.__wlbridge._v = o; };
globalThis.__wlbridge_take_wb = () => globalThis.__wlbridge.take();

// Merge wasm-bindgen's imports with wasm_lite's (no key overlap).
const imports = { ...__wbg_get_imports(), ...makeImports() };
const { instance, module } = await WebAssembly.instantiateStreaming(fetch("./program.wasm"), imports);

// Wire wasm_lite's instance BEFORE finalize: wasm-bindgen's __wbindgen_start
// (run inside finalize) calls the bin's `main`, so our imports must be ready.
setInstance(instance);
__wbg_finalize_init(instance, module);
"#;

/// Run the wasm-bindgen CLI on `input`, then assemble the interop bundle.
///
/// Requires `wasm-bindgen` on `PATH`, version-matched to the crate's
/// `wasm-bindgen` dependency.
pub fn build_interop(input: &Path) -> Result<InteropBundle, String> {
    let out_dir = std::env::temp_dir().join("wasm_lite_bindgen_out");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("creating temp dir: {e}"))?;

    let status = Command::new("wasm-bindgen")
        .arg(input)
        .args(["--target", "web", "--out-name", "app"])
        .arg("--out-dir")
        .arg(&out_dir)
        .status()
        .map_err(|e| {
            format!(
                "could not run `wasm-bindgen` (is wasm-bindgen-cli installed and \
                 version-matched to the crate?): {e}"
            )
        })?;
    if !status.success() {
        return Err("wasm-bindgen CLI failed to process the module".to_string());
    }

    let wasm = std::fs::read(out_dir.join("app_bg.wasm"))
        .map_err(|e| format!("reading wasm-bindgen output: {e}"))?;
    let wb_js = std::fs::read_to_string(out_dir.join("app.js"))
        .map_err(|e| format!("reading wasm-bindgen glue: {e}"))?;
    let _ = std::fs::remove_dir_all(&out_dir);

    // Our descriptors survive the CLI in the finalized module.
    let descriptors = descriptors_from_wasm(&wasm)?;
    let exports = crate::exports_from_wasm(&wasm)?;
    Ok(InteropBundle {
        loader_js: LOADER_JS.to_string(),
        wl_glue_js: generate_glue(&descriptors, &exports),
        wb_glue_js: patch_wasm_bindgen_glue(&wb_js),
        wasm,
    })
}

/// Adapt wasm-bindgen's `--target web` glue for the merged loader:
///   * replace `import * as importN from "<module>"` (its attempt to ES-import
///     our foreign modules) with empty objects — our `makeImports` provides the
///     real ones and wins the merge;
///   * export the two internal hooks the loader drives.
///
/// This assumes `--target web` output, where wasm-bindgen's own imports are
/// inline and only foreign modules use `import * as`.
pub fn patch_wasm_bindgen_glue(js: &str) -> String {
    let mut out = String::with_capacity(js.len() + 64);
    for line in js.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("import * as ") {
            if let Some((ident, _)) = rest.split_once(" from ") {
                out.push_str("const ");
                out.push_str(ident.trim());
                out.push_str(" = {};\n");
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\nexport { __wbg_get_imports, __wbg_finalize_init };\n");
    out
}
