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
    // `Option<T>`/`Result<T,E>` returns use a return pointer (sret): the export
    // takes a leading `__ret: *mut u8` buffer and writes a discriminant word plus
    // the payload, since a single scalar return can't carry both (an `f64` has no
    // spare bit). `is_sret` flags this so the buffer param is prepended below.
    let (ret_decl, ret_tag, ret_expr, is_sret) = match sret_return(&call, sig.ret.as_deref()) {
        Some(Ok((tag, body))) => (String::new(), tag, body, true),
        Some(Err(msg)) => return compile_error(&msg),
        None => {
            let (decl, tag, expr) = match sig.ret.as_deref() {
                None => (String::new(), String::new(), format!("{call};")),
                Some("i32") | Some("u32") | Some("f64") => {
                    let ty = sig.ret.as_deref().unwrap();
                    (format!(" -> {ty}"), ty.to_string(), call)
                }
                Some("bool") => (" -> i32".to_string(), "bool".to_string(), format!("(({call}) as i32)")),
                Some("String") => (
                    " -> i64".to_string(),
                    "str".to_string(),
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
                    "bytes".to_string(),
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
                    "handle".to_string(),
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
            (decl, tag, expr, false)
        }
    };

    // sret writes the payload into the JS-provided buffer; the export itself gains
    // a leading `__ret` pointer.
    if is_sret {
        flat_params.insert(0, "__ret: *mut u8".to_string());
    }

    // String marshalling needs the allocator exported even when the shim itself
    // doesn't call it (e.g. only `&str` args). Force-keep both. sret returns may
    // also allocate (str/bytes payloads), and JS always allocates the buffer.
    let needs_alloc = sig.params.iter().any(|(_, t)| t == "&str" || t == "&[u8]")
        || ret_tag == "str"
        || ret_tag == "bytes"
        || is_sret;
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

/// If `ret` is `Option<T>`/`Result<T,E>`, build `(descriptor_tag, body)` for an
/// sret return — code writing a discriminant word at `__ret` and the payload at
/// `__ret + 8`. `None` if not sret; `Some(Err)` if an inner type is unsupported.
///
/// Discriminant: Option uses 1=Some / 0=None; Result uses 0=Ok / 1=Err.
fn sret_return(call: &str, ret: Option<&str>) -> Option<Result<(String, String), String>> {
    let ret = ret?;
    if let Some(inner) = ret.strip_prefix("Option<").and_then(|s| s.strip_suffix('>')) {
        let inner = inner.trim();
        let (tag, write) = match payload(inner, "__x") {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        let body = format!(
            "let __v: ::core::option::Option<{inner}> = {call}; \
             match __v {{ \
                 ::core::option::Option::Some(__x) => {{ \
                     unsafe {{ ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }} {write} \
                 }} \
                 ::core::option::Option::None => unsafe {{ ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); }}, \
             }}"
        );
        return Some(Ok((format!("opt:{tag}"), body)));
    }
    if let Some(inner) = ret.strip_prefix("Result<").and_then(|s| s.strip_suffix('>')) {
        let comma = match split_top_comma(inner) {
            Some(c) => c,
            None => return Some(Err(format!("#[wasm_lite::export]: malformed return type `{ret}`"))),
        };
        let ok_ty = inner[..comma].trim();
        let err_ty = inner[comma + 1..].trim();
        let (ok_tag, ok_write) = match payload(ok_ty, "__x") {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        let (err_tag, err_write) = match payload(err_ty, "__e") {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        let body = format!(
            "let __v: ::core::result::Result<{ok_ty}, {err_ty}> = {call}; \
             match __v {{ \
                 ::core::result::Result::Ok(__x) => {{ \
                     unsafe {{ ::core::ptr::write_unaligned(__ret as *mut u32, 0u32); }} {ok_write} \
                 }} \
                 ::core::result::Result::Err(__e) => {{ \
                     unsafe {{ ::core::ptr::write_unaligned(__ret as *mut u32, 1u32); }} {err_write} \
                 }} \
             }}"
        );
        return Some(Ok((format!("res:{ok_tag}:{err_tag}"), body)));
    }
    None
}

/// Code to write `binding` (of type `ty`) into the sret buffer at `__ret + 8`
/// (str/bytes also use `__ret + 12` for the length). Returns the descriptor tag
/// and the code. Writes are unaligned (the buffer is align-1).
fn payload(ty: &str, binding: &str) -> Result<(&'static str, String), String> {
    let off8 = "(__ret as *mut u8).add(8)";
    let off12 = "(__ret as *mut u8).add(12)";
    Ok(match ty {
        "i32" => ("i32", format!("unsafe {{ ::core::ptr::write_unaligned({off8} as *mut i32, {binding}); }}")),
        "u32" => ("u32", format!("unsafe {{ ::core::ptr::write_unaligned({off8} as *mut u32, {binding}); }}")),
        "f64" => ("f64", format!("unsafe {{ ::core::ptr::write_unaligned({off8} as *mut f64, {binding}); }}")),
        "bool" => ("bool", format!("unsafe {{ ::core::ptr::write_unaligned({off8} as *mut i32, ({binding}) as i32); }}")),
        "JsValue" => (
            "handle",
            format!(
                "{{ let __h = ::wasm_lite::JsValue::__wl_abi(&{binding}); ::core::mem::forget({binding}); \
                   unsafe {{ ::core::ptr::write_unaligned({off8} as *mut u32, __h); }} }}"
            ),
        ),
        "String" | "Vec<u8>" => {
            let tag = if ty == "String" { "str" } else { "bytes" };
            (
                tag,
                format!(
                    "{{ let __len = {binding}.len(); let __ptr = ::wasm_lite::__wl_malloc(__len); \
                       unsafe {{ ::core::ptr::copy_nonoverlapping({binding}.as_ptr(), __ptr, __len); \
                       ::core::ptr::write_unaligned({off8} as *mut u32, __ptr as usize as u32); \
                       ::core::ptr::write_unaligned({off12} as *mut u32, __len as u32); }} }}"
                ),
            )
        }
        other => return Err(format!("#[wasm_lite::export]: unsupported Option/Result payload type `{other}`")),
    })
}

/// Index of the top-level `,` in a generic argument list (skips nested `<...>`).
fn split_top_comma(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
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

/// Declare a typed handle wrapper over a JS object.
///
/// ```ignore
/// wasm_lite::js_class! {
///     type JsArray;
///     impl JsArray {
///         fn push(&self, value: f64) -> f64;            // method
///         fn join(&self, sep: &str) -> String;          // &str arg, String return
///         fn concat(&self, other: &JsArray) -> JsArray; // typed arg + typed return
///     }
/// }
/// ```
///
/// Generates a newtype `struct JsArray(JsValue)` with `from_js`/`as_js`/`into_js`
/// conversions and one method per declared function. Each method lowers to a
/// `receiver[jsName](args)` call: it delegates the ABI to [`import!`] (so `&str`,
/// `&[u8]`, numbers, `bool`, and handles all marshal exactly as they do there),
/// adding only the typed veneer — object types (`&JsArray`, `-> JsArray`) cross
/// the boundary as value-table handles and are wrapped/unwrapped automatically.
///
/// Use `as "jsName"` to bind a JS name that differs from the Rust method name
/// (e.g. `fn set_attribute(&self, ...) as "setAttribute"`).
///
/// [`import!`]: wasm_lite::import
#[proc_macro]
pub fn js_class(input: TokenStream) -> TokenStream {
    match build_js_class(input) {
        Ok(ts) => ts,
        Err(msg) => compile_error(&msg),
    }
}

/// A parsed `js_class!` method: `fn name(&self, params) -> ret as "js";`.
struct Method {
    name: String,
    params: Vec<(String, String)>,
    ret: Option<String>,
    js: Option<String>,
}

fn build_js_class(input: TokenStream) -> Result<TokenStream, String> {
    let mut it = input.into_iter().peekable();

    // `type Class;`
    expect_ident(&mut it, "type")?;
    let class = next_ident(&mut it)?;
    expect_punct(&mut it, ';')?;

    // `impl Class { ... }`
    expect_ident(&mut it, "impl")?;
    let class2 = next_ident(&mut it)?;
    if class2 != class {
        return Err(format!("`impl {class2}` does not match `type {class}`"));
    }
    let body = match it.next() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g,
        _ => return Err("expected `{ ... }` after `impl`".into()),
    };
    let methods = parse_methods(body.stream())?;

    // Generate the typed wrappers and the matching `import!` method decls.
    let module = format!("__wl_class_{class}");
    let mut wrappers = String::new();
    let mut import_decls = String::new();
    for m in &methods {
        let mut imp_args = vec!["this: &JsValue".to_string()];
        let mut wrap_params = Vec::new();
        let mut call_args = vec!["self.as_js()".to_string()];
        for (n, ty) in &m.params {
            wrap_params.push(format!("{n}: {ty}"));
            match arg_kind(ty) {
                ArgKind::Passthrough => {
                    imp_args.push(format!("{n}: {ty}"));
                    call_args.push(n.clone());
                }
                ArgKind::ObjectRef => {
                    // A typed object handle: import it as `&JsValue`, lower via as_js().
                    imp_args.push(format!("{n}: &JsValue"));
                    call_args.push(format!("{n}.as_js()"));
                }
                ArgKind::Unsupported => {
                    return Err(format!(
                        "js_class method `{}`: unsupported argument type `{ty}` (object args must be `&T`)",
                        m.name
                    ));
                }
            }
        }

        let call = format!("{module}::{}({})", m.name, call_args.join(", "));
        let (wrap_ret, imp_ret, body) = match m.ret.as_deref() {
            None => (String::new(), String::new(), format!("{call};")),
            Some(ty) if is_builtin_ret(ty) => (format!(" -> {ty}"), format!(" -> {ty}"), call),
            // A typed object return: the import yields a handle; wrap it.
            Some(ty) => (format!(" -> {ty}"), " -> JsValue".to_string(), format!("{ty}::from_js({call})")),
        };

        let recv = if wrap_params.is_empty() {
            "&self".to_string()
        } else {
            format!("&self, {}", wrap_params.join(", "))
        };
        wrappers.push_str(&format!("    pub fn {}({recv}){wrap_ret} {{ {body} }}\n", m.name));

        let js = m.js.clone().unwrap_or_else(|| m.name.clone());
        import_decls.push_str(&format!(
            "            fn {}({}){imp_ret} as {js:?};\n",
            m.name,
            imp_args.join(", ")
        ));
    }

    let generated = format!(
        "pub struct {class}(::wasm_lite::JsValue);\n\
         impl {class} {{\n\
         \x20   /// Wrap a `JsValue` as this type (unchecked — no runtime type test).\n\
         \x20   pub fn from_js(v: ::wasm_lite::JsValue) -> Self {{ {class}(v) }}\n\
         \x20   /// Borrow the underlying handle.\n\
         \x20   pub fn as_js(&self) -> &::wasm_lite::JsValue {{ &self.0 }}\n\
         \x20   /// Unwrap into the underlying handle.\n\
         \x20   pub fn into_js(self) -> ::wasm_lite::JsValue {{ self.0 }}\n\
         {wrappers}\
         }}\n\
         impl ::core::convert::From<{class}> for ::wasm_lite::JsValue {{\n\
         \x20   fn from(v: {class}) -> Self {{ v.0 }}\n\
         }}\n\
         mod {module} {{\n\
         \x20   use ::wasm_lite::JsValue;\n\
         \x20   ::wasm_lite::import! {{\n\
         \x20       {class:?} {{\n\
         {import_decls}\
         \x20       }}\n\
         \x20   }}\n\
         }}\n"
    );

    TokenStream::from_str(&generated).map_err(|e| format!("js_class generated invalid code: {e}"))
}

fn parse_methods(stream: TokenStream) -> Result<Vec<Method>, String> {
    let mut it = stream.into_iter().peekable();
    let mut methods = Vec::new();
    while it.peek().is_some() {
        expect_ident(&mut it, "fn")?;
        let name = next_ident(&mut it)?;
        let params_group = match it.next() {
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => g,
            _ => return Err(format!("expected `(...)` after `fn {name}`")),
        };
        let params = parse_self_params(params_group.stream())?;
        let (ret, js) = parse_ret_and_js(&mut it)?;
        methods.push(Method { name, params, ret, js });
    }
    Ok(methods)
}

/// Parse a method's parameter list, requiring a leading `&self`.
fn parse_self_params(stream: TokenStream) -> Result<Vec<(String, String)>, String> {
    let toks: Vec<TokenTree> = stream.into_iter().collect();
    let has_self = matches!(toks.first(), Some(TokenTree::Punct(p)) if p.as_char() == '&')
        && matches!(toks.get(1), Some(TokenTree::Ident(i)) if i.to_string() == "self");
    if !has_self {
        return Err("js_class methods must take `&self` as the first parameter".into());
    }
    if toks.len() == 2 {
        return Ok(Vec::new());
    }
    if !matches!(toks.get(2), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
        return Err("expected `,` after `&self`".into());
    }
    let rest: TokenStream = toks[3..].iter().cloned().collect();
    parse_params(rest).ok_or_else(|| "could not parse method parameters".into())
}

/// Parse an optional `-> Ret` and optional `as "js"`, consuming the trailing `;`.
fn parse_ret_and_js(
    it: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
) -> Result<(Option<String>, Option<String>), String> {
    let mut ret = String::new();
    let mut js = None;
    let mut after_arrow = false;
    let mut prev_dash = false;
    while let Some(tt) = it.next() {
        match &tt {
            TokenTree::Punct(p) if p.as_char() == ';' => {
                return Ok((if ret.is_empty() { None } else { Some(ret) }, js));
            }
            TokenTree::Ident(i) if i.to_string() == "as" => match it.next() {
                Some(TokenTree::Literal(l)) => {
                    js = Some(l.to_string().trim_matches('"').to_string());
                }
                _ => return Err("expected a string literal after `as`".into()),
            },
            _ if after_arrow => ret.push_str(&tt.to_string()),
            TokenTree::Punct(p) if p.as_char() == '-' => prev_dash = true,
            TokenTree::Punct(p) if p.as_char() == '>' && prev_dash => after_arrow = true,
            _ => prev_dash = false,
        }
    }
    Err("expected `;` to end a js_class method".into())
}

/// How a method argument crosses into the underlying `import!` call.
enum ArgKind {
    /// A builtin (`&str`, `&[u8]`, `&JsValue`, numeric, `bool`): passed unchanged.
    Passthrough,
    /// A typed object handle (`&Foo`): lowered to `&JsValue` via `as_js()`.
    ObjectRef,
    Unsupported,
}

fn arg_kind(ty: &str) -> ArgKind {
    match ty {
        "&str" | "&[u8]" | "&JsValue" | "i32" | "u32" | "f64" | "bool" => ArgKind::Passthrough,
        _ if ty.starts_with('&') => ArgKind::ObjectRef,
        _ => ArgKind::Unsupported,
    }
}

/// Whether a return type is a builtin (marshalled by `import!`) vs a typed class.
fn is_builtin_ret(ty: &str) -> bool {
    matches!(ty, "i32" | "u32" | "f64" | "bool" | "String" | "Vec<u8>" | "JsValue")
}

fn expect_ident(it: &mut impl Iterator<Item = TokenTree>, want: &str) -> Result<(), String> {
    match it.next() {
        Some(TokenTree::Ident(i)) if i.to_string() == want => Ok(()),
        other => Err(format!("expected `{want}`, found {other:?}")),
    }
}

fn next_ident(it: &mut impl Iterator<Item = TokenTree>) -> Result<String, String> {
    match it.next() {
        Some(TokenTree::Ident(i)) => Ok(i.to_string()),
        other => Err(format!("expected an identifier, found {other:?}")),
    }
}

fn expect_punct(it: &mut impl Iterator<Item = TokenTree>, want: char) -> Result<(), String> {
    match it.next() {
        Some(TokenTree::Punct(p)) if p.as_char() == want => Ok(()),
        other => Err(format!("expected `{want}`, found {other:?}")),
    }
}
