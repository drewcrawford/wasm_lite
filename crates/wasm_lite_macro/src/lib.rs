//! Procedural macros for wasm_lite.
//!
//! Hand-rolled with only the built-in `proc_macro` crate — no `syn`/`quote` —
//! to honor the project's "avoid dependencies" goal. The parsing needed is
//! minimal: find the annotated function's name.

use proc_macro::{Delimiter, TokenStream, TokenTree};
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
             ::wasm_lite::set_panic_hook(); \
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

/// Export a Rust function to JavaScript callers.
///
/// ```ignore
/// #[wasm_lite::export]
/// pub fn add(a: i32, b: i32) -> i32 { a + b }
/// ```
///
/// Generates a wasm export (`__wl_export_add`) with a flattened ABI and records
/// the signature in the `__wl_exports` section, so the codegen emits a matching
/// JS wrapper: `import { add } from "./glue.js"; add(2, 3) // 5`.
///
/// v1 supports numeric (`i32`, `u32`, `f64`) and `bool` arguments and returns.
#[proc_macro_attribute]
pub fn export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let sig = match parse_signature(&item) {
        Some(sig) => sig,
        None => return compile_error("#[wasm_lite::export] must be on a plain function"),
    };

    // Validate and build the flattened ABI + the descriptor tags.
    let mut params = String::new(); // `a: i32, b: i32`
    let mut call_args = String::new(); // `a, (b != 0)`
    let mut arg_tags = String::new(); // `i32,bool`
    for (i, (name, ty)) in sig.params.iter().enumerate() {
        if i > 0 {
            params.push_str(", ");
            call_args.push_str(", ");
            arg_tags.push(',');
        }
        let Some(abi) = abi_ty(ty) else {
            return compile_error(&format!("#[wasm_lite::export]: unsupported argument type `{ty}`"));
        };
        params.push_str(&format!("{name}: {abi}"));
        call_args.push_str(&call_expr(name, ty));
        arg_tags.push_str(ty);
    }

    let (ret_decl, ret_tag, body) = match &sig.ret {
        None => (String::new(), String::new(), format!("{}({call_args})", sig.name)),
        Some(ty) => {
            let Some(abi) = abi_ty(ty) else {
                return compile_error(&format!("#[wasm_lite::export]: unsupported return type `{ty}`"));
            };
            let call = format!("{}({call_args})", sig.name);
            (format!(" -> {abi}"), ty.clone(), ret_expr(&call, ty))
        }
    };

    let name = &sig.name;
    let descriptor = format!("{name}|{arg_tags}|{ret_tag}");
    let generated = format!(
        "#[unsafe(no_mangle)] pub extern \"C\" fn __wl_export_{name}({params}){ret_decl} {{ {body} }} \
         const _: () = {{ \
             #[used] \
             #[cfg_attr(target_arch = \"wasm32\", unsafe(link_section = \"__wl_exports\"))] \
             static __WL_EXPORT: [u8; {len}] = *b\"{descriptor}\\n\"; \
         }};",
        len = descriptor.len() + 1,
    );

    let mut out = item;
    out.extend(TokenStream::from_str(&generated).expect("wasm_lite export glue should parse"));
    out
}

/// The wasm ABI type a Rust type lowers to (`bool` -> `i32`); `None` if unsupported.
fn abi_ty(ty: &str) -> Option<&'static str> {
    match ty {
        "i32" => Some("i32"),
        "u32" => Some("u32"),
        "f64" => Some("f64"),
        "bool" => Some("i32"),
        _ => None,
    }
}

/// Convert a shim parameter back to the Rust type for the call.
fn call_expr(name: &str, ty: &str) -> String {
    match ty {
        "bool" => format!("({name} != 0)"),
        _ => name.to_string(),
    }
}

/// Convert a Rust return value to the shim's ABI type.
fn ret_expr(call: &str, ty: &str) -> String {
    match ty {
        "bool" => format!("({call}) as i32"),
        _ => call.to_string(),
    }
}

/// A parsed function signature: name, `(arg, type)` pairs, and return type.
struct Signature {
    name: String,
    params: Vec<(String, String)>,
    ret: Option<String>,
}

fn parse_signature(item: &TokenStream) -> Option<Signature> {
    let mut iter = item.clone().into_iter();

    // Scan to `fn <name> ( <params> )`.
    let mut name = None;
    let mut params_group = None;
    while let Some(tt) = iter.next() {
        if let TokenTree::Ident(id) = &tt {
            if id.to_string() == "fn" {
                name = match iter.next() {
                    Some(TokenTree::Ident(n)) => Some(n.to_string()),
                    _ => return None,
                };
                params_group = match iter.next() {
                    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => Some(g),
                    _ => return None,
                };
                break;
            }
        }
    }
    let name = name?;
    let params = parse_params(params_group?.stream())?;

    // Return type: tokens after `->`, up to the body `{ ... }`.
    let mut ret = String::new();
    let mut after_arrow = false;
    for tt in iter {
        match &tt {
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => break,
            TokenTree::Punct(p) if p.as_char() == '>' => after_arrow = true,
            TokenTree::Punct(p) if p.as_char() == '-' => {}
            _ if after_arrow => ret.push_str(&tt.to_string()),
            _ => {}
        }
    }

    Some(Signature {
        name,
        params,
        ret: if ret.is_empty() { None } else { Some(ret) },
    })
}

/// Parse `a: i32, b: bool` into `[(a, i32), (b, bool)]`.
fn parse_params(stream: TokenStream) -> Option<Vec<(String, String)>> {
    let mut segments: Vec<Vec<TokenTree>> = vec![Vec::new()];
    for tt in stream {
        if matches!(&tt, TokenTree::Punct(p) if p.as_char() == ',') {
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(tt);
        }
    }

    let mut params = Vec::new();
    for seg in segments {
        if seg.is_empty() {
            continue; // trailing comma
        }
        let colon = seg
            .iter()
            .position(|tt| matches!(tt, TokenTree::Punct(p) if p.as_char() == ':'))?;
        let name = match &seg[0] {
            TokenTree::Ident(i) => i.to_string(),
            _ => return None,
        };
        let ty: String = seg[colon + 1..].iter().map(|tt| tt.to_string()).collect();
        params.push((name, ty));
    }
    Some(params)
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
