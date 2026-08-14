// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `#[wasm_bindgen(...)]` attribute arguments this shim understands.

use syn::{Error, Ident, LitStr, Path};

/// The `#[wasm_bindgen(...)]` arguments this shim understands.
#[derive(Default)]
pub(crate) struct Opts {
    pub(crate) method: bool,
    pub(crate) getter: bool,
    pub(crate) setter: bool,
    pub(crate) constructor: bool,
    pub(crate) catch: bool,
    pub(crate) indexing_getter: bool,
    pub(crate) indexing_setter: bool,
    pub(crate) indexing_deleter: bool,
    pub(crate) variadic: bool,
    pub(crate) thread_local: bool,
    pub(crate) js_name: Option<String>,
    pub(crate) js_class: Option<String>,
    pub(crate) js_namespace: Option<String>,
    pub(crate) static_method_of: Option<Ident>,
    pub(crate) extends: Vec<Path>,
    /// `is_type_of = <expr>` — a custom membership test, used where
    /// `instanceof` is wrong.
    pub(crate) is_type_of: Option<syn::Expr>,
}

impl Opts {
    /// Parse every `#[wasm_bindgen(..)]` attribute on an item, and report which
    /// of its attributes were consumed so the rest can be re-emitted.
    pub(crate) fn parse(attrs: &[syn::Attribute]) -> syn::Result<Opts> {
        let mut o = Opts::default();
        for a in attrs {
            if !is_ours(a.path()) {
                continue;
            }
            if matches!(a.meta, syn::Meta::Path(_)) {
                continue; // bare `#[wasm_bindgen]`
            }
            a.parse_nested_meta(|m| {
                let id = m
                    .path
                    .get_ident()
                    .ok_or_else(|| m.error("expected an identifier"))?
                    .clone();
                match id.to_string().as_str() {
                    "method" => o.method = true,
                    // `getter`, `getter = "name"` and `getter = name` are all
                    // in the wild; js-sys writes the bare-ident form.
                    "getter" => {
                        o.getter = true;
                        if m.input.peek(syn::Token![=]) {
                            o.js_name = Some(string_or_ident(&m)?);
                        }
                    }
                    "setter" => {
                        o.setter = true;
                        if m.input.peek(syn::Token![=]) {
                            o.js_name = Some(string_or_ident(&m)?);
                        }
                    }
                    "constructor" => o.constructor = true,
                    "catch" => o.catch = true,
                    "indexing_getter" => o.indexing_getter = true,
                    "indexing_setter" => o.indexing_setter = true,
                    "indexing_deleter" => o.indexing_deleter = true,
                    "js_name" => o.js_name = Some(string_or_ident(&m)?),
                    "js_class" => o.js_class = Some(string_or_ident(&m)?),
                    "static_method_of" => {
                        o.static_method_of = Some(m.value()?.parse::<Ident>()?);
                    }
                    "js_namespace" => o.js_namespace = Some(namespace(&m)?),
                    "extends" => o.extends.push(m.value()?.parse::<Path>()?),
                    // A *custom* membership test, replacing `instanceof`.
                    // Ignoring it is not cosmetic: js-sys uses it exactly where
                    // `instanceof` gives the wrong answer, because
                    // `"hi" instanceof String` is false for a primitive.
                    "is_type_of" => o.is_type_of = Some(m.value()?.parse()?),
                    // Accepted and ignored: these describe *how* wasm-bindgen
                    // looks a member up or what it emits for TypeScript, and
                    // wasm_lite's lowering already does the equivalent (a
                    // property lookup on the receiver) or has no TS output.
                    "structural" | "final" | "typescript_type" | "skip_typescript"
                    | "skip_jsdoc" | "getter_with_clone" | "no_deref" | "no_upcast"
                    | "no_promising" => {
                        if m.input.peek(syn::Token![=]) {
                            let _: syn::Expr = m.value()?.parse()?;
                        }
                    }
                    // Refused rather than ignored: silently dropping these
                    // generates glue that calls the wrong thing.
                    "variadic" => o.variadic = true,
                    "thread_local" | "thread_local_v2" => o.thread_local = true,
                    other @ ("module" | "raw_module" | "inline_js" | "start") => {
                        return Err(m.error(format!(
                            "#[wasm_bindgen({other})] is not supported by the wasm_lite shim yet"
                        )));
                    }
                    other => {
                        return Err(m.error(format!("unknown #[wasm_bindgen] argument `{other}`")));
                    }
                }
                Ok(())
            })?;
        }
        Ok(o)
    }
}

/// Is this attribute ours?
///
/// By the *last* segment, because the attribute is written fully qualified in
/// the wild — `#[wasm_bindgen::prelude::wasm_bindgen(method, ..)]`. Matching a
/// bare ident leaves those unconsumed, and they then re-expand on an item that
/// is no longer an extern block.
pub(crate) fn is_ours(path: &syn::Path) -> bool {
    path.segments
        .last()
        .is_some_and(|s| s.ident == "wasm_bindgen")
}

/// `js_name = "foo"` and `js_name = foo` are both in the wild.
pub(crate) fn string_or_ident(m: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    use syn::ext::IdentExt;
    let v = m.value()?;
    if v.peek(LitStr) {
        Ok(v.parse::<LitStr>()?.value())
    } else {
        // `parse_any`, not `parse`: JS member names are not constrained to Rust
        // idents, and js-sys really does write `js_name = match` and
        // `js_name = type`.
        Ok(Ident::parse_any(v)?.unraw().to_string())
    }
}

/// `js_namespace = Foo` or `js_namespace = ["Foo", "Bar"]`.
pub(crate) fn namespace(m: &syn::meta::ParseNestedMeta) -> syn::Result<String> {
    let v = m.value()?;
    if v.peek(syn::token::Bracket) {
        let content;
        syn::bracketed!(content in v);
        let parts: syn::punctuated::Punctuated<LitStr, syn::Token![,]> =
            content.parse_terminated(|p| p.parse::<LitStr>(), syn::Token![,])?;
        if parts.len() != 1 {
            // A dotted namespace would have to be `globalThis["a"]["b"]`, and
            // wasm_lite's codegen does a single lookup. Emitting it anyway
            // would produce glue that reads `globalThis["a.b"]`.
            return Err(Error::new_spanned(
                parts.first(),
                "the wasm_lite shim supports only a single-segment js_namespace",
            ));
        }
        Ok(parts[0].value())
    } else if v.peek(LitStr) {
        Ok(v.parse::<LitStr>()?.value())
    } else {
        Ok(v.parse::<Ident>()?.to_string())
    }
}
