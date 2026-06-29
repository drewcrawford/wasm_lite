//! Rust functions exported to JS via `#[wasm_lite::export]`.
//!
//! Each export is one line in the `__wl_exports` section: `name|argtags|rettag`.

/// A Rust function exported to JavaScript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The JS-callable name (the Rust fn name).
    pub name: String,
    /// Argument types, in order.
    pub args: Vec<ExportArg>,
    /// How the return value is presented to JS.
    pub ret: ExportRet,
}

/// How an exported function's argument crosses from JS into wasm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportArg {
    /// A number/bool, passed through.
    Num,
    /// A `&str`: allocate in wasm memory, write, pass `(ptr, len)`, then free.
    Str,
    /// A `&[u8]`: allocate in wasm memory, write the bytes, pass `(ptr, len)`, then free.
    Bytes,
    /// A `JsValue`: register the object in the value table, pass its index (Rust owns it).
    Handle,
    /// `Option<T>`: a discriminant param (`null`/`undefined` → 0) plus T's flattening.
    Opt(Payload),
}

/// A scalar payload inside an `Option`/`Result` sret return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    I32,
    U32,
    F64,
    Bool,
    Str,
    Bytes,
    Handle,
}

impl Payload {
    /// Number of wasm params this payload occupies (str/bytes are `(ptr, len)`).
    pub(crate) fn param_count(self) -> usize {
        match self {
            Payload::Str | Payload::Bytes => 2,
            _ => 1,
        }
    }

    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "i32" => Payload::I32,
            "u32" => Payload::U32,
            "f64" => Payload::F64,
            "bool" => Payload::Bool,
            "str" => Payload::Str,
            "bytes" => Payload::Bytes,
            "handle" => Payload::Handle,
            _ => return None,
        })
    }
}

/// How an export's return value is presented to JS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRet {
    /// No return value.
    Void,
    /// An `i32` to coerce to a JS boolean.
    Bool,
    /// A number, returned directly.
    Value,
    /// A `String`: returned as a packed `(ptr, len)` to decode and free.
    Str,
    /// A `Vec<u8>`: returned as a packed `(ptr, len)` to copy out and free.
    Bytes,
    /// A `JsValue`: returned as a value-table index to look up and free the slot.
    Handle,
    /// `Option<T>` via sret: discriminant + payload buffer; `None` → JS `null`.
    Opt(Payload),
    /// `Result<T, E>` via sret: `Ok` → value, `Err` → thrown JS exception.
    Res(Payload, Payload),
}

/// Read exported-function descriptors from a compiled wasm module.
pub fn exports_from_wasm(wasm: &[u8]) -> Result<Vec<Export>, String> {
    match crate::wasm::custom_section(wasm, "__wl_exports")? {
        Some(bytes) => parse(bytes),
        None => Ok(Vec::new()),
    }
}

fn parse(bytes: &[u8]) -> Result<Vec<Export>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("exports section is not UTF-8: {e}"))?;

    let mut exports = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('|');
        let name = fields.next().unwrap_or_default();
        let arg_tags = fields.next().unwrap_or_default();
        let ret_tag = fields.next().unwrap_or_default();

        if name.is_empty() {
            return Err(format!("malformed export line: {line:?}"));
        }

        let args = arg_tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(|t| match t {
                "str" => ExportArg::Str,
                "bytes" => ExportArg::Bytes,
                "handle" => ExportArg::Handle,
                _ => match t.strip_prefix("opt:").and_then(Payload::from_tag) {
                    Some(p) => ExportArg::Opt(p),
                    None => ExportArg::Num,
                },
            })
            .collect();
        let ret = parse_ret(ret_tag)?;

        exports.push(Export {
            name: name.to_string(),
            args,
            ret,
        });
    }
    Ok(exports)
}

/// Parse a return tag: `opt:<P>` / `res:<P>:<P>` (sret) or a plain scalar tag.
fn parse_ret(tag: &str) -> Result<ExportRet, String> {
    if let Some(inner) = tag.strip_prefix("opt:") {
        let p = Payload::from_tag(inner).ok_or_else(|| format!("bad Option payload tag {tag:?}"))?;
        return Ok(ExportRet::Opt(p));
    }
    if let Some(rest) = tag.strip_prefix("res:") {
        let (ok, err) = rest.split_once(':').ok_or_else(|| format!("bad Result tag {tag:?}"))?;
        let ok = Payload::from_tag(ok).ok_or_else(|| format!("bad Result Ok tag {tag:?}"))?;
        let err = Payload::from_tag(err).ok_or_else(|| format!("bad Result Err tag {tag:?}"))?;
        return Ok(ExportRet::Res(ok, err));
    }
    Ok(match tag {
        "" => ExportRet::Void,
        "bool" => ExportRet::Bool,
        "str" => ExportRet::Str,
        "bytes" => ExportRet::Bytes,
        "handle" => ExportRet::Handle,
        _ => ExportRet::Value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports() {
        let section = b"add|i32,i32|i32\nis_even|i32|bool\ngreet|str|str\ntick||\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "add".into(), args: vec![ExportArg::Num, ExportArg::Num], ret: ExportRet::Value },
                Export { name: "is_even".into(), args: vec![ExportArg::Num], ret: ExportRet::Bool },
                Export { name: "greet".into(), args: vec![ExportArg::Str], ret: ExportRet::Str },
                Export { name: "tick".into(), args: vec![], ret: ExportRet::Void },
            ]
        );
    }

    #[test]
    fn parses_byte_exports() {
        let section = b"sum_bytes|bytes|u32\nmake_bytes|i32|bytes\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "sum_bytes".into(), args: vec![ExportArg::Bytes], ret: ExportRet::Value },
                Export { name: "make_bytes".into(), args: vec![ExportArg::Num], ret: ExportRet::Bytes },
            ]
        );
    }

    #[test]
    fn parses_option_and_result_exports() {
        let section = b"checked_sqrt|f64|opt:f64\nfirst_word|str|opt:str\ndivide|f64,f64|res:f64:str\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "checked_sqrt".into(), args: vec![ExportArg::Num], ret: ExportRet::Opt(Payload::F64) },
                Export { name: "first_word".into(), args: vec![ExportArg::Str], ret: ExportRet::Opt(Payload::Str) },
                Export {
                    name: "divide".into(),
                    args: vec![ExportArg::Num, ExportArg::Num],
                    ret: ExportRet::Res(Payload::F64, Payload::Str),
                },
            ]
        );
    }

    #[test]
    fn parses_option_args() {
        let section = b"greet_opt|opt:str|str\nbump|opt:f64|f64\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "greet_opt".into(), args: vec![ExportArg::Opt(Payload::Str)], ret: ExportRet::Str },
                Export { name: "bump".into(), args: vec![ExportArg::Opt(Payload::F64)], ret: ExportRet::Value },
            ]
        );
    }

    #[test]
    fn parses_handle_exports() {
        let section = b"make_array|f64,f64|handle\npush_to|handle,f64|handle\n";
        assert_eq!(
            parse(section).unwrap(),
            vec![
                Export { name: "make_array".into(), args: vec![ExportArg::Num, ExportArg::Num], ret: ExportRet::Handle },
                Export { name: "push_to".into(), args: vec![ExportArg::Handle, ExportArg::Num], ret: ExportRet::Handle },
            ]
        );
    }
}
