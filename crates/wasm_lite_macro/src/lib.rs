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
/// Supported arguments: numeric (`i32`/`u32`/`f64`), `bool`, `&str`, `&[u8]`,
/// and `JsValue` (a live JS object handle). Supported returns: those, plus
/// `String`, `Vec<u8>`, and `JsValue`.
#[proc_macro_attribute]
pub fn export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let sig = match parse_signature(&item) {
        Some(sig) => sig,
        None => return compile_error("#[wasm_lite::export] must be on a plain function"),
    };

    // Build the flattened ABI params, argument reconstructions, the call, and
    // descriptor tags.
    let mut flat_params = Vec::new(); // shim parameter declarations
    let mut pre = String::new(); // statements reconstructing args before the call
    let mut call_args = Vec::new(); // arguments passed to the user function
    let mut arg_tags = Vec::new(); // descriptor tags
    for (name, ty) in &sig.params {
        match ty.as_str() {
            "&str" => {
                flat_params.push(format!("{name}_ptr: *const u8"));
                flat_params.push(format!("{name}_len: usize"));
                pre.push_str(&format!(
                    "let {name} = unsafe {{ ::core::str::from_utf8_unchecked(::core::slice::from_raw_parts({name}_ptr, {name}_len)) }};"
                ));
                call_args.push(name.clone());
                arg_tags.push("str");
            }
            "&[u8]" => {
                flat_params.push(format!("{name}_ptr: *const u8"));
                flat_params.push(format!("{name}_len: usize"));
                pre.push_str(&format!(
                    "let {name} = unsafe {{ ::core::slice::from_raw_parts({name}_ptr, {name}_len) }};"
                ));
                call_args.push(name.clone());
                arg_tags.push("bytes");
            }
            "JsValue" => {
                // JS registers the object in the value table and passes its index;
                // Rust takes ownership of the handle (and frees the slot on drop).
                flat_params.push(format!("{name}: u32"));
                pre.push_str(&format!(
                    "let {name} = ::wasm_lite::JsValue::__wl_from_abi({name});"
                ));
                call_args.push(name.clone());
                arg_tags.push("handle");
            }
            "i32" | "u32" | "f64" => {
                flat_params.push(format!("{name}: {ty}"));
                call_args.push(name.clone());
                arg_tags.push(ty);
            }
            "bool" => {
                flat_params.push(format!("{name}: i32"));
                call_args.push(format!("({name} != 0)"));
                arg_tags.push("bool");
            }
            other => {
                return compile_error(&format!("#[wasm_lite::export]: unsupported argument type `{other}`"));
            }
        }
    }

    let call = format!("{}({})", sig.name, call_args.join(", "));
    let (ret_decl, ret_tag, ret_expr) = match sig.ret.as_deref() {
        None => (String::new(), "", format!("{call};")),
        Some("i32") | Some("u32") | Some("f64") => {
            let ty = sig.ret.as_deref().unwrap();
            (format!(" -> {ty}"), ty, call)
        }
        Some("bool") => (" -> i32".to_string(), "bool", format!("(({call}) as i32)")),
        Some("String") => (
            " -> i64".to_string(),
            "str",
            // Copy into a __wl_malloc buffer JS can free, return packed (ptr, len).
            format!(
                "let __r: ::std::string::String = {call}; \
                 let __len = __r.len(); \
                 let __ptr = ::wasm_lite::__wl_malloc(__len); \
                 unsafe {{ ::core::ptr::copy_nonoverlapping(__r.as_ptr(), __ptr, __len); }} \
                 (((__ptr as usize as u64) << 32) | (__len as u64)) as i64"
            ),
        ),
        Some("Vec<u8>") => (
            " -> i64".to_string(),
            "bytes",
            // Same packing as a String, but the bytes are returned verbatim.
            format!(
                "let __r: ::std::vec::Vec<u8> = {call}; \
                 let __len = __r.len(); \
                 let __ptr = ::wasm_lite::__wl_malloc(__len); \
                 unsafe {{ ::core::ptr::copy_nonoverlapping(__r.as_ptr(), __ptr, __len); }} \
                 (((__ptr as usize as u64) << 32) | (__len as u64)) as i64"
            ),
        ),
        Some("JsValue") => (
            " -> u32".to_string(),
            "handle",
            // Hand the table slot to JS: take the index, then `forget` so Drop
            // doesn't free it — ownership transfers across the boundary.
            format!(
                "let __r: ::wasm_lite::JsValue = {call}; \
                 let __idx = ::wasm_lite::JsValue::__wl_abi(&__r); \
                 ::core::mem::forget(__r); \
                 __idx"
            ),
        ),
        Some(other) => {
            return compile_error(&format!("#[wasm_lite::export]: unsupported return type `{other}`"));
        }
    };

    // String marshalling needs the allocator exported even when the shim itself
    // doesn't call it (e.g. only `&str` args). Force-keep both.
    let needs_alloc = sig.params.iter().any(|(_, t)| t == "&str" || t == "&[u8]")
        || ret_tag == "str"
        || ret_tag == "bytes";
    let keep_alloc = if needs_alloc {
        "const _: () = { \
            #[used] static __WL_KEEP_MALLOC: extern \"C\" fn(usize) -> *mut u8 = ::wasm_lite::__wl_malloc; \
            #[used] static __WL_KEEP_FREE: extern \"C\" fn(*mut u8, usize) = ::wasm_lite::__wl_free; \
        };"
    } else {
        ""
    };

    let name = &sig.name;
    let descriptor = format!("{name}|{}|{ret_tag}", arg_tags.join(","));
    let params = flat_params.join(", ");
    let generated = format!(
        "#[unsafe(no_mangle)] pub extern \"C\" fn __wl_export_{name}({params}){ret_decl} {{ {pre} {ret_expr} }} \
         {keep_alloc} \
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

    // Return type: tokens after `->`, up to the body `{ ... }`. Detect the arrow
    // as `-` then `>` so a `>` *inside* the type (e.g. `Vec<u8>`) isn't mistaken
    // for the arrow and dropped.
    let mut ret = String::new();
    let mut after_arrow = false;
    let mut prev_dash = false;
    for tt in iter {
        match &tt {
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => break,
            _ if after_arrow => ret.push_str(&tt.to_string()),
            TokenTree::Punct(p) if p.as_char() == '-' => prev_dash = true,
            TokenTree::Punct(p) if p.as_char() == '>' && prev_dash => after_arrow = true,
            _ => prev_dash = false,
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
