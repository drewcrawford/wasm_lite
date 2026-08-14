// SPDX-License-Identifier: MIT OR Apache-2.0
//! A WebSocket echo endpoint, for testing WebSocket bindings.
//!
//! `wasm_lite::websocket` is bindings to an API that is only meaningful against
//! a peer. Without one, a test can reach the constructor, the `readyState`
//! constants and the failure path, and *nothing* that carries a byte — `send`,
//! `onmessage`, `binaryType` and the payload accessors would all ship
//! unexercised. That is the majority of the surface, and precisely the half
//! `exfiltrate` depends on.
//!
//! So the runner speaks WebSocket at one reserved path ([`PATH`]) and echoes
//! whatever it is sent. It is not a general WebSocket server and should not
//! grow into one: no extensions, no subprotocol negotiation, no permessage
//! deflate. It exists so a browser test can send a frame and get it back.
//!
//! The handshake is [RFC 6455 §4.2.2]: SHA-1 of the client's key concatenated
//! with the protocol GUID, base64-encoded. SHA-1 is implemented here because
//! the runner is deliberately dependency-free; it is used as a fixed handshake
//! function, not for security.
//!
//! [RFC 6455 §4.2.2]: https://datatracker.ietf.org/doc/html/rfc6455#section-4.2.2

use std::io::{Read, Write};
use std::net::TcpStream;

/// The path the echo endpoint answers on.
///
/// Prefixed like the runner's other generated routes so it cannot collide with
/// a file served out of `WASM_LITE_SERVE_DIR`.
pub const PATH: &str = "/__wl_echo";

/// The magic string RFC 6455 appends to the client key before hashing.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// How long a connection may sit idle before the server gives up on it.
///
/// The default 10s read timeout is right for an HTTP request and wrong for a
/// socket, where silence is the normal state between messages.
const IDLE: std::time::Duration = std::time::Duration::from_secs(120);

/// Complete the handshake and echo frames until the peer closes.
pub fn serve(mut stream: TcpStream, key: &str) -> std::io::Result<()> {
    let accept = base64(&sha1(format!("{key}{GUID}").as_bytes()));
    stream.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\
             \r\n"
        )
        .as_bytes(),
    )?;
    stream.flush()?;
    let _ = stream.set_read_timeout(Some(IDLE));

    // A message may arrive as a run of continuation frames, so the opcode of
    // the first frame is what decides how the reassembled message is echoed.
    let mut message = Vec::new();
    let mut message_opcode = 0u8;

    loop {
        let Some(frame) = read_frame(&mut stream)? else {
            return Ok(()); // peer went away without a close frame
        };
        match frame.opcode {
            // Control frames are never fragmented and never interrupt a
            // message in progress.
            0x8 => {
                // Echo the close back, which completes the handshake, then stop.
                write_frame(&mut stream, 0x8, &frame.payload)?;
                return Ok(());
            }
            0x9 => {
                write_frame(&mut stream, 0xA, &frame.payload)?; // ping -> pong
                continue;
            }
            0xA => continue, // pong; nothing to do
            0x0 => message.extend_from_slice(&frame.payload),
            op => {
                message.clear();
                message_opcode = op;
                message.extend_from_slice(&frame.payload);
            }
        }
        if frame.fin {
            write_frame(&mut stream, message_opcode, &message)?;
            message.clear();
        }
    }
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut read = 0;
    while read < buf.len() {
        match stream.read(&mut buf[read..])? {
            0 => return Ok(false),
            n => read += n,
        }
    }
    Ok(true)
}

/// Read one frame, or `None` if the peer closed the connection.
fn read_frame(stream: &mut TcpStream) -> std::io::Result<Option<Frame>> {
    let mut head = [0u8; 2];
    if !read_exact(stream, &mut head)? {
        return Ok(None);
    }
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;

    let len = match head[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            if !read_exact(stream, &mut b)? {
                return Ok(None);
            }
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            if !read_exact(stream, &mut b)? {
                return Ok(None);
            }
            u64::from_be_bytes(b)
        }
        n => n as u64,
    };
    // This server exists to bounce test payloads back; a client claiming a
    // multi-gigabyte frame is either broken or hostile, and either way the
    // answer is to hang up rather than to allocate.
    if len > 64 * 1024 * 1024 {
        return Ok(None);
    }

    let mut mask = [0u8; 4];
    if masked && !read_exact(stream, &mut mask)? {
        return Ok(None);
    }

    let mut payload = vec![0u8; len as usize];
    if !read_exact(stream, &mut payload)? {
        return Ok(None);
    }
    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Ok(Some(Frame {
        fin,
        opcode,
        payload,
    }))
}

/// Write one unfragmented frame. Server frames are never masked.
fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | opcode); // FIN
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= u16::MAX as usize => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    stream.write_all(&out)?;
    stream.flush()
}

/// SHA-1, per FIPS 180-4. Used only as the handshake's fixed hash function.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64) * 8;

    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let b = &chunk[i * 4..i * 4 + 4];
            *word = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard base64, with padding.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if group.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if group.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC's own worked example, which pins the handshake end to end.
    #[test]
    fn rfc6455_handshake_example() {
        // RFC 6455 §1.3: key "dGhlIHNhbXBsZSBub25jZQ==" must accept as
        // "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".
        let accept = base64(&sha1(format!("dGhlIHNhbXBsZSBub25jZQ=={GUID}").as_bytes()));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        // 1000 bytes: crosses several 64-byte blocks and a length field the
        // single-block cases never exercise.
        assert_eq!(
            hex(&sha1(&vec![b'a'; 1000])),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
