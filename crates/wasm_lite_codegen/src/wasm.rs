//! Minimal reader for the wasm binary format — just enough to find a custom
//! section by name. See <https://webassembly.github.io/spec/core/binary/>.

/// Return the payload of the custom section named `name`, if present.
pub fn custom_section<'a>(wasm: &'a [u8], name: &str) -> Result<Option<&'a [u8]>, String> {
    let mut r = Reader::new(wasm);
    if r.take(4)? != b"\0asm" {
        return Err("not a wasm module (bad magic)".to_string());
    }
    let _version = r.take(4)?;

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
                return Ok(Some(&body[br.pos..]));
            }
        }
    }
    Ok(None)
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

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.buf.get(self.pos).ok_or("unexpected end of wasm")?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let slice = self.buf.get(self.pos..end).ok_or("unexpected end of wasm")?;
        self.pos = end;
        Ok(slice)
    }

    /// Read an unsigned LEB128 value (used for section sizes and name lengths).
    fn leb_u32(&mut self) -> Result<u32, String> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            result |= u32::from(byte & 0x7f)
                .checked_shl(shift)
                .ok_or("LEB128 overflow")?;
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
