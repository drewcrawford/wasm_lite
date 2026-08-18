// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expansion of `extern "C"` blocks: types, statics, and their dispatch.

use crate::func::extern_fn;
use crate::opts::Opts;
use crate::ty::{cfg_attrs, passthrough_attrs, raise_ret, shim_ret_ty};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, ForeignItem, ForeignItemType, Ident, LitStr};

/// Expand an `extern "C"` block, item by item.
///
/// A failing item becomes a `compile_error!` **in place** rather than aborting
/// the block. That matters more than it sounds: a block declares a type and
/// then its members, so failing the whole block over one unsupported method
/// deletes the type too, and every binding elsewhere that mentions it fails as
/// well. Most of the errors this shim reported against js-sys were that
/// cascade rather than distinct problems.
pub(crate) fn extern_block(fm: syn::ItemForeignMod) -> syn::Result<TokenStream2> {
    // The types this block declares. A namespaced free function whose
    // namespace *is* one of them is reached as an associated function —
    // js-sys writes `Uint8Array::copy_to_slice(..)` for a binding declared
    // with `js_namespace = Uint8Array`.
    let declared: Vec<Ident> = fm
        .items
        .iter()
        .filter_map(|i| match i {
            ForeignItem::Type(t) => Some(t.ident.clone()),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for item in fm.items {
        let expanded = match item {
            ForeignItem::Type(t) => extern_type(t),
            ForeignItem::Fn(f) => extern_fn(f, &declared),
            ForeignItem::Static(st) => extern_static(st),
            other => Err(Error::new_spanned(
                other,
                "#[wasm_bindgen]: only `type` and `fn` items are supported in an extern block",
            )),
        };
        out.push(expanded.unwrap_or_else(|e| e.to_compile_error()));
    }
    Ok(quote! { #(#out)* })
}

/// `pub type Foo;` → a transparent newtype over a handle.
pub(crate) fn extern_type(t: ForeignItemType) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&t.attrs)?;
    let name = &t.ident;
    let vis = &t.vis;
    let attrs = passthrough_attrs(&t.attrs);

    // js-sys declares generic extern types (`pub type Generator<T>;`), so the
    // parameters have to reach the struct. They are phantom — the handle is
    // untyped at the ABI — but `PhantomData` is a ZST, so `repr(transparent)`
    // still holds over the one real field.
    let generics = &t.generics;
    let (impl_g, ty_g, where_g) = t.generics.split_for_impl();
    let phantom = t.generics.type_params().map(|p| {
        let id = &p.ident;
        quote! { ::wasm_bindgen::__rt::core::marker::PhantomData<#id> }
    });
    let phantom_field = if t.generics.type_params().next().is_some() {
        quote! { _params: (#(#phantom,)*), }
    } else {
        quote! {}
    };
    let phantom_init = if t.generics.type_params().next().is_some() {
        quote! { _params: ::wasm_bindgen::__rt::core::default::Default::default(), }
    } else {
        quote! {}
    };

    // `extends = Base` gives the inherited API. One `Deref` target is possible
    // in Rust, so the first `extends` becomes `Deref` (which chains, so the
    // whole ancestry is reachable) and the rest become `AsRef`.
    // A type with no `extends` derefs to `JsValue`, so every chain bottoms out
    // there — which is what lets `JsValue::as_ref(&some_deep_type)` resolve.
    let root_deref = opts.extends.is_empty().then(|| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::ops::Deref for #name #ty_g #where_g {
                type Target = ::wasm_bindgen::__rt::JsValue;
                fn deref(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
            }
        }
    });

    let deref = opts.extends.first().map(|base| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::ops::Deref for #name #ty_g #where_g {
                type Target = #base;
                fn deref(&self) -> &#base {
                    // SAFETY: every type this macro generates is
                    // `#[repr(transparent)]` over the same `JsValue`, so the
                    // reference is valid at either type. The JS value really
                    // being a `#base` is the binding author's claim, exactly as
                    // it is under wasm-bindgen.
                    unsafe { &*(self as *const #name as *const #base) }
                }
            }
        }
    });
    let as_refs = opts.extends.iter().map(|base| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::convert::AsRef<#base> for #name #ty_g #where_g {
                fn as_ref(&self) -> &#base {
                    // SAFETY: as above.
                    unsafe { &*(self as *const #name as *const #base) }
                }
            }
        }
    });

    // The upcast the `AsRef` above does not give: `let o: Object = arr.into()`.
    // wasm-bindgen emits one of these per declared `extends` next to the
    // `AsRef`, and callers rely on it — `glow` writes
    // `js_sys::Uint8Array::view(bytes).into()` where an `Object` is wanted.
    // Emitted per *declared* base, exactly as upstream: web-sys spells out the
    // whole ancestry on each type, so the transitive closure is covered without
    // this walking it (which would duplicate impls).
    let upcasts = opts.extends.iter().map(|base| {
        quote! {
            impl #impl_g ::wasm_bindgen::__rt::core::convert::From<#name #ty_g> for #base #where_g {
                fn from(v: #name #ty_g) -> #base {
                    // The claim is the same one `Deref`/`AsRef` above make, and
                    // matches wasm-bindgen: an upcast is unchecked.
                    <#base as ::wasm_bindgen::JsCast>::unchecked_from_js(
                        ::wasm_bindgen::JsObject::into_js(v),
                    )
                }
            }
        }
    });

    // The JS class this type names, for `instanceof`. web-sys renames freely
    // (`pub type HtmlElement` ↔ `HTMLElement`), so `js_name` wins.
    let js_class = opts.js_name.clone().unwrap_or_else(|| name.to_string());
    let js_class_lit = LitStr::new(&js_class, name.span());
    let cast_mod = format_ident!("__wb_instanceof_{}", name);

    // `is_type_of` replaces the `instanceof` test entirely — that is the whole
    // point of it — so the binding is not emitted at all in that case.
    let (cast_mod_def, cast_test) = match &opts.is_type_of {
        Some(expr) => (
            quote! {},
            // Coerced through a `fn` pointer so a bare closure — js-sys writes
            // `is_type_of = |v| v.is_bigint()` — knows its parameter type.
            // Every such predicate is non-capturing, so the coercion holds.
            quote! {
                {
                    let __test: fn(&::wasm_bindgen::__rt::JsValue) -> bool = #expr;
                    __test(val)
                }
            },
        ),
        None => (
            quote! {
                #[allow(non_snake_case, unused_imports)]
                mod #cast_mod {
                    use ::wasm_bindgen::__rt::JsValue;
                    ::wasm_bindgen::__rt::import! {
                        crate = ::wasm_bindgen::__rt;
                        #js_class_lit {
                            #[instanceof]
                            fn shim(this: &JsValue) -> bool as #js_class_lit;
                        }
                    }
                }
            },
            quote! { #cast_mod::shim(val) },
        ),
    };

    Ok(quote! {
        #(#attrs)*
        #[repr(transparent)]
        #vis struct #name #generics {
            obj: ::wasm_bindgen::__rt::JsValue,
            #phantom_field
        }

        impl #impl_g ::wasm_bindgen::JsObject for #name #ty_g #where_g {
            fn as_js(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
            fn from_js(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
            fn into_js(self) -> ::wasm_bindgen::__rt::JsValue { self.obj }
        }

        #cast_mod_def

        impl #impl_g ::wasm_bindgen::JsCast for #name #ty_g #where_g {
            fn instanceof(val: &::wasm_bindgen::__rt::JsValue) -> bool {
                #cast_test
            }
            fn unchecked_from_js(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
        }

        // `JsValue::from(&x)` for this type. The blanket impl lives in
        // wasm_lite, since only the owning crate may implement `From` for
        // `JsValue`; this is the hook it keys on.
        impl #impl_g ::wasm_bindgen::JsArg for #name #ty_g #where_g {
            fn js_arg(&self) -> ::wasm_bindgen::JsArgRef<'_> {
                ::wasm_bindgen::JsArgRef::Borrowed(&self.obj)
            }
        }

        impl #impl_g ::wasm_bindgen::FromJs for #name #ty_g #where_g {
            fn from_js_value(obj: ::wasm_bindgen::__rt::JsValue) -> Self {
                #name { obj, #phantom_init }
            }
        }

        impl #impl_g ::wasm_bindgen::__rt::AsJsValue for #name #ty_g #where_g {
            fn as_js_value(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
        }

        // The conversions upstream code expects on every binding type.
        impl #impl_g ::wasm_bindgen::__rt::core::convert::AsRef<::wasm_bindgen::__rt::JsValue> for #name #ty_g #where_g {
            fn as_ref(&self) -> &::wasm_bindgen::__rt::JsValue { &self.obj }
        }

        impl #impl_g ::wasm_bindgen::__rt::core::convert::From<#name #ty_g> for ::wasm_bindgen::__rt::JsValue #where_g {
            fn from(v: #name #ty_g) -> ::wasm_bindgen::__rt::JsValue { v.obj }
        }

        /// Unchecked, matching wasm-bindgen. No explicit `TryFrom` alongside
        /// it: `From` induces the blanket one, and declaring both collides.
        /// Use `JsCast::dyn_into` for the checked conversion.
        impl #impl_g ::wasm_bindgen::__rt::core::convert::From<::wasm_bindgen::__rt::JsValue> for #name #ty_g #where_g {
            fn from(obj: ::wasm_bindgen::__rt::JsValue) -> Self { #name { obj, #phantom_init } }
        }

        #root_deref
        #deref
        #(#as_refs)*
        #(#upcasts)*
    })
}

/// `pub static PI: f64;` — a constant on a JS namespace, e.g. `Math.PI`.
///
/// Emitted as a function rather than a Rust `static`, because reading it is a
/// call into JS: there is nothing to initialise at load time.
pub(crate) fn extern_static(st: syn::ForeignItemStatic) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&st.attrs)?;
    let attrs = passthrough_attrs(&st.attrs);
    let name = &st.ident;
    let vis = &st.vis;
    let ty = &st.ty;

    let js_name = opts.js_name.clone().unwrap_or_else(|| name.to_string());
    let namespace = opts
        .js_namespace
        .clone()
        .or_else(|| opts.js_class.clone())
        .unwrap_or_else(|| "globalThis".to_string());
    let ns_lit = LitStr::new(&namespace, name.span());
    let js_lit = LitStr::new(&js_name, name.span());
    let module = format_ident!("__wb_static_{}", name);

    let shim_ret = shim_ret_ty(ty);
    let body = raise_ret(ty, quote! { #module::shim() });
    let cfgs = cfg_attrs(&st.attrs);

    let shim_mod = quote! {
        #(#cfgs)*
        #[allow(non_snake_case, unused_imports)]
        mod #module {
            use ::wasm_bindgen::__rt::JsValue;
            ::wasm_bindgen::__rt::import! {
                crate = ::wasm_bindgen::__rt;
                #ns_lit {
                    #[static_getter]
                    fn shim() -> #shim_ret as #js_lit;
                }
            }
        }
    };

    if !opts.thread_local {
        return Ok(quote! {
            #shim_mod
            #(#attrs)*
            #[allow(non_snake_case)]
            #vis fn #name() -> #ty { #body }
        });
    }

    // `thread_local_v2` asks for a value accessed through `.with(|v| ..)`
    // rather than a call. The property is re-read on each access, which is the
    // honest reading: it lives in JS and could change.
    let holder = format_ident!("__WbThreadLocal_{}", name);
    Ok(quote! {
        #shim_mod

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #vis struct #holder;

        impl #holder {
            /// Read the JS property and hand it to `f`.
            pub fn with<R>(&self, f: impl ::wasm_bindgen::__rt::core::ops::FnOnce(&#ty) -> R) -> R {
                f(&{ #body })
            }
        }

        #(#attrs)*
        #[allow(non_upper_case_globals, non_snake_case)]
        #vis static #name: #holder = #holder;
    })
}
