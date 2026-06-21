//! Procedural macros for wasm_lite.
//!
//! Hand-rolled with only the built-in `proc_macro` crate — no `syn`/`quote` —
//! to honor the project's "avoid dependencies" goal. The parsing needed is
//! minimal: find the annotated function's name.

use proc_macro::{TokenStream, TokenTree};
use std::str::FromStr;

/// Mark a function as a wasm_lite test (analogous to `#[test]`).
///
/// Generates an exported `__wl_test_<name>` entry point (which installs the
/// panic hook and calls the test) and records the test's name in the
/// `__wasm_lite_tests` section so the runner discovers and drives it. Pair with
/// a one-time [`wasm_lite::test_main!`] in a `harness = false` test target.
#[proc_macro_attribute]
pub fn wasm_lite_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let name = match fn_name(item.clone()) {
        Some(name) => name,
        None => return compile_error("#[wasm_lite_test] must be applied to a function"),
    };

    // `*b"<name>\n"` is `[u8; name.len() + 1]`; concatenates into the section.
    let len = name.len() + 1;
    let generated = format!(
        "#[unsafe(no_mangle)] pub extern \"C\" fn __wl_test_{name}() {{ \
             ::wasm_lite::__set_panic_hook(); \
             {name}(); \
         }} \
         const _: () = {{ \
             #[used] \
             #[cfg_attr(target_arch = \"wasm32\", unsafe(link_section = \"__wasm_lite_tests\"))] \
             static __WL_TEST_NAME: [u8; {len}] = *b\"{name}\\n\"; \
         }};"
    );

    let mut out = item;
    out.extend(TokenStream::from_str(&generated).expect("wasm_lite_test glue should parse"));
    out
}

/// Find the name of the function in a token stream (the ident after `fn`).
fn fn_name(item: TokenStream) -> Option<String> {
    let mut iter = item.into_iter();
    while let Some(tt) = iter.next() {
        if let TokenTree::Ident(id) = tt {
            if id.to_string() == "fn" {
                return match iter.next() {
                    Some(TokenTree::Ident(name)) => Some(name.to_string()),
                    _ => None,
                };
            }
        }
    }
    None
}

fn compile_error(message: &str) -> TokenStream {
    TokenStream::from_str(&format!("compile_error!({message:?});")).unwrap()
}
