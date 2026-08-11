// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal reader for the wasm binary format — just enough to find a custom
//! section by name. See <https://webassembly.github.io/spec/core/binary/>.

/// Return the payload of the custom section named `name`, if present.
pub fn custom_section<'a>(wasm: &'a [u8], name: &str) -> Result<Option<&'a [u8]>, String> {
    Ok(custom_sections(wasm, name)?.into_iter().next())
}

/// Return every custom section named `name`, in module order.
///
/// Custom section names are not unique in the wasm format. Descriptor-bearing
/// sections can therefore be repeated by a linker or another wasm transform;
/// callers must not silently discard every occurrence after the first one.
pub fn custom_sections<'a>(wasm: &'a [u8], name: &str) -> Result<Vec<&'a [u8]>, String> {
    let mut r = Reader::new(wasm);
    r.header()?;
    let mut matches = Vec::new();

    while !r.eof() {
        let id = r.byte()?;
        let size = r.leb_u32()? as usize;
        let body = r.take(size)?;

        // Section id 0 is a custom section: a name followed by raw contents.
        if id == 0 {
            let mut br = Reader::new(body);
            let name_len = br.leb_u32()? as usize;
            let section_name = br.take(name_len)?;
            if section_name == name.as_bytes() {
                matches.push(&body[br.pos..]);
            }
        }
    }
    Ok(matches)
}

/// An imported memory, as produced by linking with `--import-memory`
/// (e.g. shared-memory `+atomics` builds, where JS creates the `WebAssembly.Memory`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MemoryImport {
    /// Import module (LLD uses `env`).
    pub module: String,
    /// Import field name (LLD uses `memory`).
    pub name: String,
    /// Initial size in 64 KiB pages.
    pub initial: u32,
    /// Maximum size in pages (required for shared memory).
    pub maximum: Option<u32>,
    /// Whether the memory is `shared` (backed by a `SharedArrayBuffer`).
    pub shared: bool,
}

/// Find the module's imported memory, if any.
///
/// Modules that define+export their own memory (the default) return `None`; a
/// `--import-memory` build (shared `+atomics`) returns the memory's limits so
/// the glue can create a matching `WebAssembly.Memory` and supply it.
pub fn imported_memory(wasm: &[u8]) -> Result<Option<MemoryImport>, String> {
    let mut r = Reader::new(wasm);
    r.header()?;
    let mut memory = None;
    let mut saw_import_section = false;

    while !r.eof() {
        let id = r.byte()?;
        let size = r.leb_u32()? as usize;
        let body = r.take(size)?;
        // Section id 2 is the import section.
        if id == 2 {
            if saw_import_section {
                return Err("multiple import sections in wasm module".to_string());
            }
            saw_import_section = true;
            memory = parse_imports_for_memory(body)?;
        }
    }
    Ok(memory)
}

/// Scan an import section's body for a memory import.
fn parse_imports_for_memory(body: &[u8]) -> Result<Option<MemoryImport>, String> {
    let mut r = Reader::new(body);
    let count = r.leb_u32()?;
    let mut memory = None;
    for _ in 0..count {
        let module = r.name()?;
        let name = r.name()?;
        // Import kind: 0=func, 1=table, 2=memory, 3=global.
        match r.byte()? {
            0x00 => {
                r.leb_u32()?; // type index
            }
            0x01 => {
                r.byte()?; // reftype
                r.skip_limits()?;
            }
            0x02 => {
                let (initial, maximum, shared) = r.read_limits()?;
                let found = MemoryImport {
                    module,
                    name,
                    initial,
                    maximum,
                    shared,
                };
                if memory.replace(found).is_some() {
                    return Err("multiple imported memories are not supported".to_string());
                }
            }
            0x03 => {
                r.byte()?; // valtype
                r.byte()?; // mutability
            }
            other => return Err(format!("unknown import kind {other} in import section")),
        }
    }
    if !r.eof() {
        return Err("trailing bytes in import section".to_string());
    }
    Ok(memory)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn header(&mut self) -> Result<(), String> {
        if self.take(4)? != b"\0asm" {
            return Err("not a wasm module (bad magic)".to_string());
        }
        if self.take(4)? != b"\x01\0\0\0" {
            return Err("unsupported wasm version".to_string());
        }
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.buf.get(self.pos).ok_or("unexpected end of wasm")?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or("unexpected end of wasm")?;
        self.pos = end;
        Ok(slice)
    }

    /// Read a name: a LEB128 length followed by that many UTF-8 bytes.
    fn name(&mut self) -> Result<String, String> {
        let len = self.leb_u32()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|e| format!("import name is not UTF-8: {e}"))
    }

    /// Read a limits descriptor: `(min, max, shared)`.
    ///
    /// Flags bit 0 = has-max, bit 1 = shared (threads proposal), bit 2 = 64-bit.
    fn read_limits(&mut self) -> Result<(u32, Option<u32>, bool), String> {
        let flags = self.byte()?;
        if flags & !0x03 != 0 {
            return Err(format!("unsupported memory limits flags {flags:#x}"));
        }
        let has_max = flags & 0x01 != 0;
        let shared = flags & 0x02 != 0;
        if shared && !has_max {
            return Err("shared memory requires a maximum".to_string());
        }
        let min = self.leb_u32()?;
        let max = if has_max { Some(self.leb_u32()?) } else { None };
        if max.is_some_and(|max| max < min) {
            return Err("memory maximum is smaller than its minimum".to_string());
        }
        Ok((min, max, shared))
    }

    /// Skip a limits descriptor (for table imports we don't care about).
    fn skip_limits(&mut self) -> Result<(), String> {
        let flags = self.byte()?;
        if flags & !0x01 != 0 {
            return Err(format!("unsupported table limits flags {flags:#x}"));
        }
        self.leb_u32()?;
        if flags & 0x01 != 0 {
            self.leb_u32()?;
        }
        Ok(())
    }

    /// Read an unsigned LEB128 value (used for section sizes and name lengths).
    fn leb_u32(&mut self) -> Result<u32, String> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            let bits = u32::from(byte & 0x7f);
            // In the 5th byte (shift 28) only the low 4 bits fit in a u32.
            // `checked_shl` wouldn't catch this — it only rejects shift >= 32,
            // silently dropping the shifted-out bits.
            if shift == 28 && bits > 0x0f {
                return Err("LEB128 overflow".to_string());
            }
            result |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 32 {
                return Err("LEB128 too long".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_module(name: &str, payloads: &[&[u8]]) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        for payload in payloads {
            let body_len = 1 + name.len() + payload.len();
            assert!(name.len() < 128 && body_len < 128);
            wasm.extend([0, body_len as u8, name.len() as u8]);
            wasm.extend(name.as_bytes());
            wasm.extend(*payload);
        }
        wasm
    }

    #[test]
    fn finds_shared_imported_memory() {
        // A module whose only section is an import of `env.memory`, a shared
        // memory with initial 17 pages and max 16384 (LEB `80 80 01`).
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic + version
            0x02, 0x12, // import section, body length 18
            0x01, // one import
            0x03, b'e', b'n', b'v', // module "env"
            0x06, b'm', b'e', b'm', b'o', b'r', b'y', // name "memory"
            0x02, // kind: memory
            0x03, 0x11, 0x80, 0x80, 0x01, // limits: has_max|shared, min 17, max 16384
        ];
        assert_eq!(
            imported_memory(wasm).unwrap(),
            Some(MemoryImport {
                module: "env".into(),
                name: "memory".into(),
                initial: 17,
                maximum: Some(16384),
                shared: true,
            })
        );
    }

    #[test]
    fn no_imported_memory_when_absent() {
        let wasm: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(imported_memory(wasm).unwrap(), None);
    }

    #[test]
    fn returns_every_matching_custom_section() {
        let wasm = custom_module("x", &[b"first", b"second"]);
        assert_eq!(
            custom_sections(&wasm, "x").unwrap(),
            [&b"first"[..], &b"second"[..]]
        );
    }

    #[test]
    fn matching_custom_section_does_not_hide_a_malformed_tail() {
        let mut wasm = custom_module("x", &[b"payload"]);
        wasm.push(1); // section id without its required size
        assert!(custom_sections(&wasm, "x").is_err());
    }

    #[test]
    fn rejects_invalid_memory_limits() {
        // Shared memory without a maximum is invalid and cannot be passed to
        // WebAssembly.Memory's `shared: true` constructor.
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x02, 0x0f, 0x01, 0x03, b'e', b'n',
            b'v', 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x02, 0x01,
        ];
        assert!(imported_memory(wasm).is_err());
    }
}
