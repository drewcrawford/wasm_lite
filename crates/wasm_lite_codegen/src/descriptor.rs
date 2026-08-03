// SPDX-License-Identifier: MIT OR Apache-2.0
//! The descriptor format written by the `import!` macro.
//!
//! Each import is one line: `kind|namespace|import_name|js_name|argtags|rettag\n`,
//! where `argtags` is a comma-separated list (possibly empty) and `rettag` is
//! empty for a binding that returns nothing. `kind` is one of:
//!
//! | tag | [`Kind`] | JS |
//! |---|---|---|
//! | `f` | [`Kind::Function`] | `globalThis[ns][js_name](args)` |
//! | `m` | [`Kind::Method`] | `receiver[js_name](args)` |
//! | `g` | [`Kind::Getter`] | `receiver[js_name]` |
//! | `s` | [`Kind::Setter`] | `receiver[js_name] = value` |
//! | `n` | [`Kind::Constructor`] | `new globalThis[js_name](args)` |
//! | `ig` | [`Kind::IndexGet`] | `receiver[index]` |
//! | `is` | [`Kind::IndexSet`] | `receiver[index] = value` |
//!
//! `import_name` is the wasm import symbol (unique per binding — it carries the
//! crate/module path); `js_name` is the JavaScript function the shim actually
//! calls. They differ for overloads, where several Rust functions bind the same
//! JS function.

use crate::exports::Payload;

/// What JavaScript operation an import performs.
///
/// [`Kind::Function`] and [`Kind::Method`] are calls; the rest are the
/// non-call operations a JS binding surface needs — property access, `new`,
/// and computed indexing. They exist because a property is not a
/// zero-argument method: `el.tagName` and `el.tagName()` are different
/// programs, and only the first one works.
///
/// There is deliberately no separate static-method kind. A static method is
/// `Klass.method(args)`, which is exactly [`Kind::Function`] with the class as
/// the namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// `globalThis[namespace][js_name](args)`.
    Function,
    /// `receiver[js_name](args)`, where the first argument is the handle receiver.
    Method,
    /// `receiver[js_name]` — a property read. One argument (the receiver), and
    /// a return value, since a getter that discards its result is pointless.
    Getter,
    /// `receiver[js_name] = value` — a property write. Two arguments (receiver,
    /// value) and no return.
    Setter,
    /// `new globalThis[js_name](args)` — a constructor. The namespace keys the
    /// import-object slot only; the class comes from `js_name` so that
    /// `namespace` stays free to group a class's bindings together.
    Constructor,
    /// `receiver[index]` — computed property read. Two arguments (receiver,
    /// index) and a return value.
    IndexGet,
    /// `receiver[index] = value` — computed property write. Three arguments
    /// (receiver, index, value) and no return.
    IndexSet,
}

/// The return marshalling of an import.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ret {
    /// No return value.
    Void,
    /// A JS object: store it in the value table and return the handle.
    Handle,
    /// A JS string: allocate it in wasm memory and return a packed `(ptr, len)`.
    Str,
    /// JS bytes: copy them into wasm memory and return a packed `(ptr, len)`.
    Bytes,
    /// `Option<T>` via sret: the shim writes a discriminant + payload to a buffer.
    Opt(Payload),
    /// `Result<T, E>` via sret: success vs. a caught JS exception.
    Res(Payload, Payload),
    /// A primitive returned directly (the tag is kept for documentation).
    Value(String),
}

/// A single imported JavaScript function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Descriptor {
    /// Which JS operation this binding performs.
    pub kind: Kind,
    /// JS namespace, e.g. `console` (unused for methods, but keys the slot).
    pub namespace: String,
    /// The wasm import name; keys the import object slot.
    pub import_name: String,
    /// The JavaScript function name the shim calls (may differ from `import_name`).
    pub js_name: String,
    /// Argument types, in declaration order (for methods, the first is the receiver).
    pub args: Vec<AbiArg>,
    /// How the return value is marshalled.
    pub ret: Ret,
}

/// How an argument crosses the wasm boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AbiArg {
    /// A `&str`: arrives as two wasm params `(ptr, len)`; decode from memory.
    Str,
    /// A `&[u8]`: arrives as two wasm params `(ptr, len)`; a memory view, no decode.
    Bytes,
    /// A `bool`: arrives as one `i32`; present to JS as a real boolean.
    Bool,
    /// A signed/float numeric type (`i32`, `f64`): one param, passed through.
    Num,
    /// A `u32`: one param; wasm i32 params surface in JS as *signed* Numbers,
    /// so the shim must reinterpret with `>>> 0` before handing it to JS.
    U32,
    /// A `&JsValue`: arrives as one `u32` index; look up in the value table.
    Handle,
    /// `Option<T>`: a discriminant `i32` (0 = `None`) plus T's params.
    Opt(Payload),
}

impl AbiArg {
    /// Number of wasm-level parameters this argument occupies.
    pub fn param_count(self) -> usize {
        match self {
            AbiArg::Str | AbiArg::Bytes => 2,
            AbiArg::Bool | AbiArg::Num | AbiArg::U32 | AbiArg::Handle => 1,
            AbiArg::Opt(p) => 1 + p.param_count(),
        }
    }

    // Unknown tags are a hard error: the macro ABI and this parser must stay in
    // lockstep, and a silent fallback would give the shim the wrong wasm-level
    // arity, shifting every subsequent argument.
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "str" => AbiArg::Str,
            "bytes" => AbiArg::Bytes,
            "bool" => AbiArg::Bool,
            "handle" => AbiArg::Handle,
            "i32" | "f64" => AbiArg::Num,
            "u32" => AbiArg::U32,
            _ => {
                return tag
                    .strip_prefix("opt:")
                    .and_then(Payload::from_tag)
                    .map(AbiArg::Opt);
            }
        })
    }
}

/// Parse the descriptor section's bytes into a list of descriptors.
pub fn parse(bytes: &[u8]) -> Result<Vec<Descriptor>, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|e| format!("descriptor section is not UTF-8: {e}"))?;

    let mut descriptors = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split('|');
        let kind_tag = fields.next().unwrap_or_default();
        let namespace = fields.next().unwrap_or_default();
        let import_name = fields.next().unwrap_or_default();
        let js_name = fields.next().unwrap_or_default();
        let arg_tags = fields.next().unwrap_or_default();
        let ret_tag = fields.next().unwrap_or_default();

        if namespace.is_empty() || import_name.is_empty() || js_name.is_empty() {
            return Err(format!("malformed descriptor line: {line:?}"));
        }

        let kind = match kind_tag {
            "f" => Kind::Function,
            "m" => Kind::Method,
            "g" => Kind::Getter,
            "s" => Kind::Setter,
            "n" => Kind::Constructor,
            "ig" => Kind::IndexGet,
            "is" => Kind::IndexSet,
            other => return Err(format!("unknown import kind {other:?} in {line:?}")),
        };

        let args: Vec<AbiArg> = arg_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(|t| {
                AbiArg::from_tag(t).ok_or_else(|| format!("unknown argument tag {t:?} in {line:?}"))
            })
            .collect::<Result<_, _>>()?;

        let ret = parse_ret(ret_tag)?;
        check_shape(kind, &args, &ret, import_name)?;

        descriptors.push(Descriptor {
            kind,
            namespace: namespace.to_string(),
            import_name: import_name.to_string(),
            js_name: js_name.to_string(),
            args,
            ret,
        });
    }

    Ok(descriptors)
}

/// Check that a descriptor's arity and return match what its kind can emit.
///
/// The generator indexes into `args` positionally — receiver, index, value —
/// so a descriptor of the wrong shape would either panic there or silently
/// produce glue that reads the wrong argument. Rejecting it here keeps the
/// failure at parse time, where the offending line can be named.
fn check_shape(kind: Kind, args: &[AbiArg], ret: &Ret, import_name: &str) -> Result<(), String> {
    let needs_receiver = matches!(
        kind,
        Kind::Method | Kind::Getter | Kind::Setter | Kind::IndexGet | Kind::IndexSet
    );
    if needs_receiver && args.first() != Some(&AbiArg::Handle) {
        return Err(format!("{kind:?} {import_name:?} needs a handle receiver"));
    }

    // (exact arity, must return a value) for the fixed-shape kinds.
    let expect = match kind {
        Kind::Getter => Some((1, true)),
        Kind::Setter => Some((2, false)),
        Kind::IndexGet => Some((2, true)),
        Kind::IndexSet => Some((3, false)),
        Kind::Function | Kind::Method | Kind::Constructor => None,
    };
    if let Some((arity, returns_value)) = expect {
        if args.len() != arity {
            return Err(format!(
                "{kind:?} {import_name:?} takes exactly {arity} argument(s), got {}",
                args.len()
            ));
        }
        if returns_value && *ret == Ret::Void {
            return Err(format!("{kind:?} {import_name:?} must return a value"));
        }
        if !returns_value && *ret != Ret::Void {
            return Err(format!("{kind:?} {import_name:?} must not return a value"));
        }
    }

    // A constructor that did not yield the constructed object would be a leak
    // with no way to reach it.
    if kind == Kind::Constructor
        && !matches!(
            ret,
            Ret::Handle | Ret::Res(Payload::Handle, _) | Ret::Opt(Payload::Handle)
        )
    {
        return Err(format!(
            "constructor {import_name:?} must return a handle, got {ret:?}"
        ));
    }

    Ok(())
}

/// Parse a return tag: `opt:<P>` / `res:<P>:<P>` (sret) or a plain scalar tag.
fn parse_ret(tag: &str) -> Result<Ret, String> {
    if let Some(inner) = tag.strip_prefix("opt:") {
        let p =
            Payload::from_tag(inner).ok_or_else(|| format!("bad Option payload tag {tag:?}"))?;
        return Ok(Ret::Opt(p));
    }
    if let Some(rest) = tag.strip_prefix("res:") {
        let (ok, err) = rest
            .split_once(':')
            .ok_or_else(|| format!("bad Result tag {tag:?}"))?;
        let ok = Payload::from_tag(ok).ok_or_else(|| format!("bad Result Ok tag {tag:?}"))?;
        let err = Payload::from_tag(err).ok_or_else(|| format!("bad Result Err tag {tag:?}"))?;
        return Ok(Ret::Res(ok, err));
    }
    Ok(match tag {
        "" => Ret::Void,
        "handle" => Ret::Handle,
        "str" => Ret::Str,
        "bytes" => Ret::Bytes,
        scalar @ ("i32" | "u32" | "f64" | "bool") => Ret::Value(scalar.to_string()),
        other => return Err(format!("unknown return tag {other:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_functions_methods_and_handles() {
        let section = b"f|console|c::log|log|str|\n\
                        f|JSON|c::parse|parse|str|handle\n\
                        m|Array|c::push|push|handle,f64|f64\n";
        let got = parse(section).unwrap();
        assert_eq!(
            got,
            vec![
                Descriptor {
                    kind: Kind::Function,
                    namespace: "console".into(),
                    import_name: "c::log".into(),
                    js_name: "log".into(),
                    args: vec![AbiArg::Str],
                    ret: Ret::Void,
                },
                Descriptor {
                    kind: Kind::Function,
                    namespace: "JSON".into(),
                    import_name: "c::parse".into(),
                    js_name: "parse".into(),
                    args: vec![AbiArg::Str],
                    ret: Ret::Handle,
                },
                Descriptor {
                    kind: Kind::Method,
                    namespace: "Array".into(),
                    import_name: "c::push".into(),
                    js_name: "push".into(),
                    args: vec![AbiArg::Handle, AbiArg::Num],
                    ret: Ret::Value("f64".into()),
                },
            ]
        );
    }

    #[test]
    fn parses_byte_slices_and_vecs() {
        let section = b"f|c|c::write|write|bytes|\nf|c|c::read|read||bytes\n";
        let got = parse(section).unwrap();
        assert_eq!(got[0].args, vec![AbiArg::Bytes]);
        assert_eq!(got[0].ret, Ret::Void);
        assert_eq!(got[1].args, vec![]);
        assert_eq!(got[1].ret, Ret::Bytes);
    }

    #[test]
    fn parses_option_and_result_returns() {
        let section = b"f|JSON|c::parse_num|parse|str|opt:f64\n\
                        f|JSON|c::try_parse|parse|str|res:f64:handle\n";
        let got = parse(section).unwrap();
        assert_eq!(got[0].ret, Ret::Opt(Payload::F64));
        assert_eq!(got[1].ret, Ret::Res(Payload::F64, Payload::Handle));
    }

    #[test]
    fn parses_option_args() {
        let section = b"f|Number|c::parse_int|parseInt|str,opt:f64|f64\n\
                        m|Array|c::join_opt|join|handle,opt:str|str\n";
        let got = parse(section).unwrap();
        assert_eq!(got[0].args, vec![AbiArg::Str, AbiArg::Opt(Payload::F64)]);
        assert_eq!(got[1].args, vec![AbiArg::Handle, AbiArg::Opt(Payload::Str)]);
    }

    #[test]
    fn rejects_method_without_receiver() {
        let section = b"m|Array|c::bad|bad|f64|\n";
        assert!(parse(section).is_err());
    }

    #[test]
    fn parses_property_constructor_and_indexing_kinds() {
        let section = b"g|Element|c::tag_name|tagName|handle|str\n\
                        s|Element|c::set_scroll|scrollTop|handle,f64|\n\
                        n|URL|c::new_url|URL|str|handle\n\
                        ig|Array|c::at|at|handle,u32|handle\n\
                        is|Array|c::put|put|handle,u32,handle|\n";
        let got = parse(section).unwrap();
        let kinds: Vec<Kind> = got.iter().map(|d| d.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Kind::Getter,
                Kind::Setter,
                Kind::Constructor,
                Kind::IndexGet,
                Kind::IndexSet
            ]
        );
    }

    /// The generator indexes these kinds' arguments positionally, so a
    /// wrong-shaped descriptor must not reach it.
    #[test]
    fn rejects_misshapen_property_bindings() {
        let cases: &[(&[u8], &str)] = &[
            (
                b"g|E|c::g|p|handle,f64|str\n",
                "getter with an extra argument",
            ),
            (b"g|E|c::g|p|handle|\n", "getter returning nothing"),
            (b"s|E|c::s|p|handle,f64|f64\n", "setter returning a value"),
            (b"s|E|c::s|p|handle|\n", "setter with no value argument"),
            (b"g|E|c::g|p|f64|str\n", "getter without a handle receiver"),
            (b"ig|E|c::i|p|handle|handle\n", "index get with no index"),
            (b"is|E|c::i|p|handle,u32|\n", "index set with no value"),
            (
                b"n|U|c::n|URL|str|str\n",
                "constructor not returning a handle",
            ),
        ];
        for (section, what) in cases {
            assert!(parse(section).is_err(), "should reject {what}");
        }
    }
}
