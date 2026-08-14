// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expansion of one imported function into an `import!` plus its wrapper.

use crate::opts::Opts;
use crate::ty::*;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, FnArg, ForeignItemFn, Ident, LitStr, Pat, PatType, ReturnType, Type};

/// One imported function.
///
/// Emits a private module holding the `import!`, plus a wrapper that converts
/// newtypes to and from handles. The wrapper is an **inherent method** when the
/// attributes say so — `method`/`getter`/`setter` become `impl Recv { fn .. }`,
/// `constructor`/`static_method_of` become associated functions — because that
/// is what wasm-bindgen produces, and it is why callers write
/// `element.tag_name()` rather than `tag_name(&element)`.
pub(crate) fn extern_fn(f: ForeignItemFn, declared: &[Ident]) -> syn::Result<TokenStream2> {
    let opts = Opts::parse(&f.attrs)?;
    let name = &f.sig.ident;
    let vis = &f.vis;
    let attrs = passthrough_attrs(&f.attrs);

    // A constructor's JS name is the *class*, not the Rust function — every
    // web-sys constructor is spelled `fn new()`, and `new globalThis["new"]`
    // is a TypeError. Fall back to the constructed type's name when `js_class`
    // is absent, which is what wasm-bindgen does.
    let js_name = opts.js_name.clone().unwrap_or_else(|| {
        if opts.constructor {
            opts.js_class
                .clone()
                .unwrap_or_else(|| match &f.sig.output {
                    ReturnType::Type(_, ty) => type_ident(constructed_ty(ty)),
                    ReturnType::Default => name.to_string(),
                })
        } else {
            name.to_string().trim_start_matches("r#").to_string()
        }
    });

    // A `getter` hung off a class rather than an instance (`Symbol.iterator`)
    // reads a *namespaced* property, with nothing to read it from.
    let static_property = opts.getter && opts.static_method_of.is_some();

    let kind_attr = if opts.constructor {
        Some(quote!(#[constructor]))
    } else if static_property {
        Some(quote!(#[static_getter]))
    } else if opts.getter {
        Some(quote!(#[getter]))
    } else if opts.setter {
        Some(quote!(#[setter]))
    } else if opts.indexing_getter {
        Some(quote!(#[indexing_getter]))
    } else if opts.indexing_setter {
        Some(quote!(#[indexing_setter]))
    } else if opts.indexing_deleter {
        Some(quote!(#[indexing_deleter]))
    } else {
        None
    };

    // Anything that reads or writes a member of a receiver takes one.
    let takes_receiver = !static_property
        && (opts.method
            || opts.getter
            || opts.setter
            || opts.indexing_getter
            || opts.indexing_setter
            || opts.indexing_deleter);

    let mut wrapper_params = Vec::new();
    let mut shim_params = Vec::new();
    let mut call_args = Vec::new();
    // Statements that must run before the call (callback adapters).
    let mut prelude: Vec<TokenStream2> = Vec::new();
    let mut receiver_ty: Option<Type> = None;

    for (i, arg) in f.sig.inputs.iter().enumerate() {
        let FnArg::Typed(PatType { pat, ty, .. }) = arg else {
            return Err(Error::new_spanned(
                arg,
                "#[wasm_bindgen]: unexpected `self`",
            ));
        };
        let Pat::Ident(pi) = &**pat else {
            return Err(Error::new_spanned(
                pat,
                "#[wasm_bindgen]: parameters must be plain identifiers",
            ));
        };
        let orig_ident = &pi.ident;

        if takes_receiver && i == 0 {
            // The declared `this: &Foo` becomes `&self`, and wasm_lite spells
            // the receiver `this` on its side.
            receiver_ty = Some(deref_ty(ty).clone());
            wrapper_params.push(quote! { &self });
            shim_params.push(quote! { this: &JsValue });
            call_args.push(quote! { ::wasm_bindgen::JsObject::as_js(self) });
            continue;
        }

        wrapper_params.push(quote! { #orig_ident: #ty });

        // A borrowed callback becomes a variadic Closure for the duration of
        // the call.
        if let Some(cb) = callback_signature(ty) {
            let unpacked = cb.inputs.iter().enumerate().map(|(i, t)| unpack_arg(t, i));
            let (body, fallible) =
                pack_result(cb.output.as_ref(), quote! { __f( #(#unpacked),* ) })?;
            let ctor = if fallible {
                quote!(new_variadic_fallible)
            } else {
                quote!(new_variadic)
            };
            let closure_ident = format_ident!("__cb_{}", orig_ident);
            let static_ty = cb.static_ty();
            prelude.push(quote! {
                let #closure_ident = {
                    // SAFETY: the Closure is dropped at the end of this
                    // function, before the caller's borrow ends, so the
                    // 'static bound it requires is never actually observed.
                    let __f: #static_ty = unsafe { ::wasm_bindgen::__rt::core::mem::transmute(#orig_ident) };
                    ::wasm_bindgen::__rt::Closure::#ctor(
                        move |__args: &[::wasm_bindgen::__rt::JsValue]| #body,
                    )
                };
            });
            shim_params.push(quote! { #orig_ident: &JsValue });
            call_args.push(quote! { #closure_ident.as_js_value() });
            continue;
        }

        if let Some(inner) = clamped_inner(ty) {
            let by_ref = matches!(&**ty, Type::Reference(_));
            let shim_ty = if by_ref {
                quote! { &#inner }
            } else {
                quote! { #inner }
            };
            shim_params.push(quote! { #orig_ident: #shim_ty });
            call_args.push(if by_ref {
                quote! { &#orig_ident.0 }
            } else {
                quote! { #orig_ident.0 }
            });
            continue;
        }

        // `Option<&Element>` is a nullable *handle*: `import!` takes
        // `Option<&JsValue>`, so only the element type needs converting.
        if let Some(inner) = generic_inner(ty, "Option")
            && matches!(classify(deref_ty(inner)), Cross::Handle)
        {
            shim_params
                .push(quote! { #orig_ident: ::wasm_bindgen::__rt::core::option::Option<&JsValue> });
            let borrowed = if matches!(inner, Type::Reference(_)) {
                quote! { #orig_ident }
            } else {
                quote! { #orig_ident.as_ref() }
            };
            call_args.push(quote! {
                #borrowed.map(::wasm_bindgen::JsObject::as_js)
            });
            continue;
        }

        match classify(deref_ty(ty)) {
            Cross::Direct => {
                shim_params.push(quote! { #orig_ident: #ty });
                call_args.push(quote! { #orig_ident });
            }
            // A slice of newtypes: every element is `#[repr(transparent)]`
            // over a `JsValue`, so the run is already the layout `import!`
            // wants and only the element type has to be reinterpreted.
            Cross::Handle if matches!(deref_ty(ty), Type::Slice(_)) => {
                shim_params.push(quote! { #orig_ident: &[JsValue] });
                call_args.push(quote! {
                    // SAFETY: `T` is one of the `#[repr(transparent)]`
                    // newtypes this macro generates, so `[T]` and `[JsValue]`
                    // have the same layout.
                    unsafe {
                        ::wasm_bindgen::__rt::core::slice::from_raw_parts(
                            #orig_ident.as_ptr() as *const ::wasm_bindgen::__rt::JsValue,
                            #orig_ident.len(),
                        )
                    }
                });
            }
            Cross::Handle => {
                // Handles are always *lent* to an import, so the shim takes a
                // reference whether the declared parameter was one or not —
                // which means only a by-value parameter needs the extra `&`.
                shim_params.push(quote! { #orig_ident: &JsValue });
                let borrow = if matches!(&**ty, Type::Reference(_)) {
                    quote! { #orig_ident }
                } else {
                    quote! { &#orig_ident }
                };
                call_args.push(quote! { &*::wasm_bindgen::JsArg::js_arg(#borrow) });
            }
        }
    }

    // The namespace keys the import-object slot; for a method it is the class,
    // for a static the class the method hangs off, for a free function the JS
    // namespace (or the global object).
    let namespace = opts
        .js_class
        .clone()
        .or_else(|| opts.static_method_of.as_ref().map(|i| i.to_string()))
        .or_else(|| opts.js_namespace.clone())
        .unwrap_or_else(|| "globalThis".to_string());
    let ns_lit = LitStr::new(&namespace, name.span());
    let js_lit = LitStr::new(&js_name, name.span());

    // Where the wrapper hangs: the receiver for a method, the named class for a
    // static, the constructed type for a constructor.
    let impl_target: Option<Type> = if takes_receiver {
        receiver_ty
    } else if let Some(cls) = &opts.static_method_of {
        Some(syn::parse_quote!(#cls))
    } else if opts.constructor {
        match &f.sig.output {
            ReturnType::Type(_, ty) => Some(constructed_ty(ty).clone()),
            ReturnType::Default => None,
        }
    } else if let Some(ns) = &opts.js_namespace
        && let Some(id) = declared.iter().find(|d| *d == ns)
    {
        Some(syn::parse_quote!(#id))
    } else {
        None
    };

    // Module names must not collide when two classes share a member name.
    let bare = name.to_string();
    let bare = bare.trim_start_matches("r#");
    // Hashed, not just named: js-sys generates the same member on many types
    // from one macro, and `__wb_<Type>_<name>` still collides when the
    // generated `impl` targets share a spelling. The signature is what
    // actually distinguishes them.
    let disambiguator = token_hash(&quote! { #impl_target #ns_lit #js_lit #(#shim_params)* });
    let module = match &impl_target {
        Some(t) => format_ident!("__wb_{}_{}_{:x}", type_ident(t), bare, disambiguator),
        None => format_ident!("__wb_{}_{:x}", bare, disambiguator),
    };
    let call = quote! { #module::shim( #(#call_args),* ) };

    let (wrapper_ret, shim_ret, body) = match &f.sig.output {
        ReturnType::Default => (quote! {}, quote! {}, call),
        ReturnType::Type(_, ty) => (
            quote! { -> #ty },
            {
                let s = shim_ret_ty(ty);
                quote! { -> #s }
            },
            raise_ret(ty, call),
        ),
    };

    let variadic_attr = opts.variadic.then(|| quote!(#[variadic]));
    let cfgs = cfg_attrs(&f.attrs);
    let shim_mod = quote! {
        #(#cfgs)*
        #[allow(non_snake_case, unused_imports)]
        mod #module {
            use ::wasm_bindgen::__rt::JsValue;
            ::wasm_bindgen::__rt::import! {
                crate = ::wasm_bindgen::__rt;
                #ns_lit {
                    #kind_attr
                    #variadic_attr
                    fn shim( #(#shim_params),* ) #shim_ret as #js_lit;
                }
            }
        }
    };

    let (impl_generics, fn_generics) = split_generics(&f.sig.generics, impl_target.as_ref());
    let (generics, where_clause) = wrapper_generics(&fn_generics);
    let wrapper = quote! {
        #(#attrs)*
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #vis fn #name #generics ( #(#wrapper_params),* ) #wrapper_ret #where_clause {
            #(#prelude)*
            #body
        }
    };

    Ok(match impl_target {
        Some(t) => {
            let (ig, _, _) = impl_generics.split_for_impl();
            quote! {
                #shim_mod
                impl #ig #t { #wrapper }
            }
        }
        None => quote! {
            #shim_mod
            #wrapper
        },
    })
}
