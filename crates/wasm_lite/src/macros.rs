//! The `import!` macro and its internal helpers.
//!
//! `import!` turns a declaration like
//!
//! ```ignore
//! wasm_lite::import! {
//!     "console" {
//!         fn log(msg: &str);
//!         fn error(msg: &str);
//!     }
//! }
//! ```
//!
//! into, for each function:
//!
//!   * a safe Rust wrapper (`pub fn log(msg: &str)`),
//!   * a function-local wasm import declaration with a *flattened* ABI
//!     (`&str` becomes `*const u8, usize`), and
//!   * a line in the `__wasm_lite_imports` custom wasm section describing the
//!     import so the host-side codegen can generate a matching JS shim.
//!
//! Multiple `#[link_section]` statics with the same name concatenate, so each
//! `import!` invocation contributes its descriptors to one shared section.
//!
//! Supported argument types: `&str`, `bool`, and numeric idents (`i32`, `u32`,
//! `f64`, ...). Supported return types: none, `bool`, or a numeric ident.
//! Only one `import!` invocation is allowed per module (the descriptor static
//! has a fixed name).

/// Declare imported JavaScript functions grouped by JS namespace.
#[macro_export]
macro_rules! import {
    (
        $(
            $ns:literal {
                $(
                    fn $fname:ident ( $($args:tt)* ) $( -> $ret:ident )? $( as $jsname:literal )? ;
                )*
            }
        )*
    ) => {
        // Safe wrappers + flattened imports, one per function. The wrapper and
        // its wasm import are keyed on the Rust fn name; the JS call target
        // (which may differ, to allow overloads) is recorded in the descriptor.
        $( $(
            $crate::__import_fn!($ns, $fname, ( $($args)* ) $( -> $ret )? );
        )* )*

        // Descriptors for this invocation: `ns|name|argtags|rettag\n` per fn.
        //
        // Wrapped in an anonymous `const _: () = { ... }` so multiple `import!`
        // calls can coexist in one module: each `const _` and its inner items
        // are independently scoped, so the fixed names don't collide. (Only the
        // public wrapper fns share the module's namespace, so the only real
        // conflict left is declaring the same function name twice.)
        const _: () = {
            // `kind|ns|import_name|js_name|argtags|rettag\n`. kind is `f`/`m`.
            // import_name is the wasm import symbol — made unique per (crate,
            // module, fn) via `module_path!()` so independent crates never
            // collide; it must match the `#[link_name]` above exactly. js_name
            // is what the shim calls.
            const DESCR_STR: &str = concat!( $( $(
                $crate::__import_kind!($($args)*), "|",
                $ns, "|", concat!(module_path!(), "::", stringify!($fname)), "|",
                $crate::__js_name!($fname $(, $jsname)?), "|",
                $crate::__import_descr_args!($($args)*), "|",
                $crate::__import_descr_ret!($( $ret )?), "\n",
            )* )* );

            // The descriptor section only matters for the wasm build; on other
            // targets `link_section` names like this are invalid (e.g. mach-O
            // wants `segment,section`), so restrict it to wasm.
            #[used]
            #[cfg_attr(target_arch = "wasm32", unsafe(link_section = "__wasm_lite_imports"))]
            static DESCR: [u8; DESCR_STR.len()] =
                $crate::descriptor_bytes::<{ DESCR_STR.len() }>(DESCR_STR);
        };
    };
}

/// Emit one safe wrapper + its flattened, function-local wasm import.
#[doc(hidden)]
#[macro_export]
macro_rules! __import_fn {
    // Entry: start munching the argument list into flattened forms.
    ($ns:literal, $fname:ident, ( $($args:tt)* ) $( -> $ret:ident )? ) => {
        $crate::__import_fn!(@munch
            ns = $ns, name = $fname, ret = ( $( $ret )? ),
            orig = ( $($args)* ), flat = ( ), call = ( ),
            rest = ( $($args)* )
        );
    };

    // &str -> (*const u8, usize)
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : & str , $($rest:tt)* )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: *const u8, _: usize,),
            call=($($call)* $a.as_ptr(), $a.len(),), rest=( $($rest)* ));
    };
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : & str )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: *const u8, _: usize,),
            call=($($call)* $a.as_ptr(), $a.len(),), rest=( ));
    };

    // &JsValue -> u32 (a borrowed value-table handle)
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : & JsValue , $($rest:tt)* )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: u32,),
            call=($($call)* $a.__wl_abi(),), rest=( $($rest)* ));
    };
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : & JsValue )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: u32,),
            call=($($call)* $a.__wl_abi(),), rest=( ));
    };

    // bool -> i32
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : bool , $($rest:tt)* )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: i32,),
            call=($($call)* $a as i32,), rest=( $($rest)* ));
    };
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : bool )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: i32,),
            call=($($call)* $a as i32,), rest=( ));
    };

    // numeric ident (i32, u32, f64, ...) -> itself
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : $t:ident , $($rest:tt)* )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: $t,),
            call=($($call)* $a,), rest=( $($rest)* ));
    };
    (@munch ns=$ns:literal, name=$fname:ident, ret=($($ret:ident)?),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( $a:ident : $t:ident )) => {
        $crate::__import_fn!(@munch ns=$ns, name=$fname, ret=($($ret)?),
            orig=($($orig)*), flat=($($flat)* _: $t,),
            call=($($call)* $a,), rest=( ));
    };

    // Terminal: no return.
    (@munch ns=$ns:literal, name=$fname:ident, ret=(),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( )) => {
        pub fn $fname($($orig)*) {
            #[link(wasm_import_module = $ns)]
            unsafe extern "C" {
                #[link_name = concat!(module_path!(), "::", stringify!($fname))]
                fn $fname($($flat)*);
            }
            unsafe { $fname($($call)*) }
        }
    };
    // Terminal: bool return (i32 at the ABI).
    (@munch ns=$ns:literal, name=$fname:ident, ret=(bool),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( )) => {
        pub fn $fname($($orig)*) -> bool {
            #[link(wasm_import_module = $ns)]
            unsafe extern "C" {
                #[link_name = concat!(module_path!(), "::", stringify!($fname))]
                fn $fname($($flat)*) -> i32;
            }
            unsafe { $fname($($call)*) != 0 }
        }
    };
    // Terminal: JsValue return (a value-table handle: ABI is the u32 index).
    (@munch ns=$ns:literal, name=$fname:ident, ret=(JsValue),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( )) => {
        pub fn $fname($($orig)*) -> $crate::JsValue {
            #[link(wasm_import_module = $ns)]
            unsafe extern "C" {
                #[link_name = concat!(module_path!(), "::", stringify!($fname))]
                fn $fname($($flat)*) -> u32;
            }
            $crate::JsValue::__wl_from_abi(unsafe { $fname($($call)*) })
        }
    };
    // Terminal: numeric return.
    (@munch ns=$ns:literal, name=$fname:ident, ret=($ret:ident),
            orig=($($orig:tt)*), flat=($($flat:tt)*), call=($($call:tt)*),
            rest=( )) => {
        pub fn $fname($($orig)*) -> $ret {
            #[link(wasm_import_module = $ns)]
            unsafe extern "C" {
                #[link_name = concat!(module_path!(), "::", stringify!($fname))]
                fn $fname($($flat)*) -> $ret;
            }
            unsafe { $fname($($call)*) }
        }
    };
}

/// Build the comma-separated argument type tags for a descriptor line.
#[doc(hidden)]
#[macro_export]
macro_rules! __import_descr_args {
    () => { "" };
    ( $a:ident : & str ) => { "str" };
    ( $a:ident : & str , $($rest:tt)* ) => {
        concat!("str,", $crate::__import_descr_args!($($rest)*))
    };
    ( $a:ident : & JsValue ) => { "handle" };
    ( $a:ident : & JsValue , $($rest:tt)* ) => {
        concat!("handle,", $crate::__import_descr_args!($($rest)*))
    };
    ( $a:ident : bool ) => { "bool" };
    ( $a:ident : bool , $($rest:tt)* ) => {
        concat!("bool,", $crate::__import_descr_args!($($rest)*))
    };
    ( $a:ident : $t:ident ) => { stringify!($t) };
    ( $a:ident : $t:ident , $($rest:tt)* ) => {
        concat!(stringify!($t), ",", $crate::__import_descr_args!($($rest)*))
    };
}

/// Build the return type tag for a descriptor line (empty for no return).
#[doc(hidden)]
#[macro_export]
macro_rules! __import_descr_ret {
    () => { "" };
    ( JsValue ) => { "handle" };
    ( $r:ident ) => { stringify!($r) };
}

/// Pick the JS call target: an explicit `as "name"` override, or the Rust name.
#[doc(hidden)]
#[macro_export]
macro_rules! __js_name {
    ($fname:ident) => { stringify!($fname) };
    ($fname:ident, $js:literal) => { $js };
}

/// Classify an import: `m` (method) if the first param is `this: &JsValue`,
/// otherwise `f` (a namespaced free function).
#[doc(hidden)]
#[macro_export]
macro_rules! __import_kind {
    ( this : & JsValue $($rest:tt)* ) => { "m" };
    ( $($other:tt)* ) => { "f" };
}
