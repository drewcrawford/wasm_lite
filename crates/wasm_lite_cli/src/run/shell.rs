// SPDX-License-Identifier: MIT OR Apache-2.0
//! The interactive HTML shell, and the console output it forwards back.
//!
//! Two halves of one contract, kept together because they have to agree: the
//! browser-side [`CONSOLE_FORWARDER`] encodes `console` records and POSTs them
//! to [`LOG_PATH`](super::LOG_PATH), and [`print_log_batch`] decodes them onto
//! the terminal that ran `cargo run`. A change to the wire format is a change
//! to both.

use std::path::Path;

/// The HTML shell that loads the program as an ES module from `entry`.
///
/// The shell contributes **no elements** to the document. It used to open with
/// a `<pre>` that mirrored console output onto the page, which made the shell a
/// layout participant: a program appending its own element to `<body>` — a
/// canvas, say — got it placed *after* the log, and every line logged pushed it
/// further down the page. A host shell has to hand the program the same empty
/// document it will get from the page it ships with, so program output goes to
/// the terminal instead (see [`CONSOLE_FORWARDER`]).
pub(super) fn index_html(program: &Path, entry: &str) -> String {
    let title = program
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program");
    format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <title>wasm_lite runner — {title}</title>\n\
         </head>\n\
         <body>\n\
         <script>{CONSOLE_FORWARDER}</script>\n\
         <script type=\"module\" src=\"{entry}\"></script>\n\
         </body>\n\
         </html>\n"
    )
}

/// Classic (non-module) script that forwards `console` output to the runner,
/// which prints it on the terminal that ran `cargo run`. Runs before the module,
/// so it catches what the program logs on its way up.
///
/// This is what makes `cargo run --target wasm32-unknown-unknown` behave like a
/// native `cargo run`: the program in the window, its output in the shell you
/// started it from. Worker output arrives too — generated worker glue bridges
/// each worker's console into the main realm, which is where this is installed.
///
/// Four properties are load-bearing, and each is a bug if dropped:
///
/// * **Batched.** A program that logs per frame would otherwise issue a request
///   per line at 60Hz. One request per 50ms of lines carries the same output
///   for a fraction of the main-thread time.
/// * **One request in flight.** Each connection is served on its own thread, so
///   overlapping POSTs would print out of order.
/// * **Escaped records.** One record is one line. A panic message carries a
///   multi-line stack trace, which unescaped would be read as several records
///   with garbage level tags.
/// * **Silent on failure.** A rejected `fetch` is reported through
///   `console.error` — the very thing this hooks — so an unreachable server
///   would feed itself forever.
///
/// The endpoint is [`LOG_PATH`](super::LOG_PATH); the two have to agree, which
/// `forwarder_posts_to_the_log_path` checks.
pub(super) const CONSOLE_FORWARDER: &str = r#"
(() => {
    const ENDPOINT = "/__wl_log";
    const FLUSH_MS = 50;
    const MAX_PENDING = 512;

    let pending = [];
    let inflight = false;
    let timer = null;

    const encode = (level, text) =>
        level + " " +
        text.replace(/\\/g, "\\\\").replace(/\n/g, "\\n").replace(/\r/g, "\\r") +
        "\n";

    const flush = () => {
        if (inflight || pending.length === 0) return;
        const body = pending.join("");
        pending = [];
        inflight = true;
        fetch(ENDPOINT, { method: "POST", body })
            .catch(() => {})
            .then(() => {
                inflight = false;
                if (pending.length) flush();
            });
    };

    for (const level of ["log", "error", "warn", "info", "debug"]) {
        const original = console[level].bind(console);
        console[level] = (...args) => {
            original(...args);
            pending.push(encode(level, args.join(" ")));
            if (pending.length >= MAX_PENDING) {
                flush();
            } else if (timer === null) {
                timer = setTimeout(() => { timer = null; flush(); }, FLUSH_MS);
            }
        };
    }

    // A page being torn down drops whatever is still buffered, and the lines
    // just before a crash or a navigation are the ones worth having. `fetch`
    // does not survive unload; `sendBeacon` does.
    addEventListener("pagehide", () => {
        if (pending.length === 0) return;
        navigator.sendBeacon(ENDPOINT, pending.join(""));
        pending = [];
    });

    const LOGWISE_ENDPOINT = "/__wl_logwise";
    const MAX_STRUCTURED_BYTES = 512 * 1024;
    let structured = [];
    let structuredBytes = 0;
    let structuredInflight = false;
    let structuredTimer = null;
    const flushStructured = () => {
        if (structuredInflight || structured.length === 0) return;
        const body = new Blob(structured.map(bytes => new Uint8Array(bytes)));
        structured = [];
        structuredBytes = 0;
        structuredInflight = true;
        fetch(LOGWISE_ENDPOINT, { method: "POST", body })
            .catch(() => {})
            .then(() => {
                structuredInflight = false;
                if (structured.length) flushStructured();
            });
    };
    addEventListener("__wl_logwise", event => {
        structured.push(event.detail);
        structuredBytes += event.detail.length;
        if (structured.length >= MAX_PENDING || structuredBytes >= MAX_STRUCTURED_BYTES) {
            flushStructured();
        } else if (structuredTimer === null) {
            structuredTimer = setTimeout(() => {
                structuredTimer = null;
                flushStructured();
            }, FLUSH_MS);
        }
    });
    addEventListener("pagehide", () => {
        if (structured.length === 0) return;
        navigator.sendBeacon(
            LOGWISE_ENDPOINT,
            new Blob(structured.map(bytes => new Uint8Array(bytes))),
        );
        structured = [];
        structuredBytes = 0;
    });
})();
"#;

/// Print one batch of forwarded console records.
///
/// The level decides the stream, following the browser's own split:
/// `console.log` and `console.info` are output, `console.warn` and
/// `console.error` are diagnostics. That keeps `cargo run > out.txt` collecting
/// what the program printed while leaving what went wrong on the terminal —
/// the same division a native `cargo run` gives you.
///
/// Nothing is prefixed. A logging framework has already formatted its level
/// into the message, and the runner adding a second one would be noise.
pub(super) fn print_log_batch(body: &[u8]) {
    for (level, text) in decode_log_batch(&String::from_utf8_lossy(body)) {
        match level {
            "warn" | "error" => eprintln!("{text}"),
            _ => println!("{text}"),
        }
    }
}

/// Print a batch of concatenated, self-framed `logwise_v1` envelopes.
pub(super) fn print_logwise_batch(mut body: &[u8]) {
    while body.len() >= 12 {
        let length = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        if length < 12 || length > body.len() {
            break;
        }
        if let Some(summary) = summarize_logwise(&body[..length]) {
            println!("{summary}");
        }
        body = &body[length..];
    }
}

fn summarize_logwise(bytes: &[u8]) -> Option<String> {
    if bytes.get(..4)? != b"LW1\0" || read_u16(bytes, 4)? != 1 {
        return None;
    }
    let sequence = read_u64(bytes, 12)?;
    let dropped = read_u64(bytes, 20)?;
    let truncated = read_u64(bytes, 28)?;
    let worker = read_u64(bytes, 36)?;
    let context = read_u64(bytes, 44)?;
    let mut position = 60;
    let links = read_u16(bytes, position)? as usize;
    position = position.checked_add(2 + links.checked_mul(16)?)?;
    position = position.checked_add(3)?; // severity, class, kind
    let event = read_text(bytes, &mut position)?;
    read_text(bytes, &mut position)?; // package
    read_text(bytes, &mut position)?; // target
    read_text(bytes, &mut position)?; // module
    if read_u8(bytes, &mut position)? != 0 {
        read_text(bytes, &mut position)?; // optional domain
    }
    let test = if read_u8(bytes, &mut position)? != 0 {
        Some(read_text(bytes, &mut position)?)
    } else {
        None
    };
    let test = test.map_or(String::new(), |test| format!(" test={test}"));
    Some(format!(
        "logwise[{sequence}] {event}{test} worker={worker} context={context} dropped={dropped} truncated={truncated}"
    ))
}

fn read_u8(bytes: &[u8], position: &mut usize) -> Option<u8> {
    let value = *bytes.get(*position)?;
    *position = position.checked_add(1)?;
    Some(value)
}

fn read_u16(bytes: &[u8], position: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(position..position + 2)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], position: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(position..position + 8)?.try_into().ok()?,
    ))
}

fn read_text<'a>(bytes: &'a [u8], position: &mut usize) -> Option<&'a str> {
    let length = read_u16(bytes, *position)? as usize;
    *position = position.checked_add(2)?;
    let text = std::str::from_utf8(bytes.get(*position..position.checked_add(length)?)?).ok()?;
    *position += length;
    Some(text)
}

/// Split a forwarded batch into `(level, text)` records.
///
/// The wire format is one record per line, `<level> <escaped-text>`, with `\`,
/// newline and carriage return escaped by the forwarder so that a multi-line
/// message (a panic's stack trace, say) stays one record.
///
/// Only `\n`-terminated lines are records: a batch clipped at
/// [`MAX_REQUEST_BODY`](super::MAX_REQUEST_BODY) ends mid-record, and half a line under a mangled level
/// is a worse answer than no line.
fn decode_log_batch(batch: &str) -> Vec<(&str, String)> {
    batch
        .split_inclusive('\n')
        .filter_map(|record| record.strip_suffix('\n'))
        .map(|record| {
            // A record with no space is a level with an empty message, not a
            // malformed one — `console.log()` with no arguments produces it.
            let (level, text) = record.split_once(' ').unwrap_or((record, ""));
            (level, unescape_log_text(text))
        })
        .collect()
}

/// Undo the forwarder's escaping.
///
/// An unrecognized or trailing escape cannot occur in a well-formed record, so
/// it is passed through verbatim rather than treated as an error: this is a
/// debugging channel, and printing a line slightly wrong beats dropping it.
fn unescape_log_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{LOG_PATH, PROGRAM_JS};

    /// The forwarder and the server have to agree on the endpoint, and they are
    /// written in different languages a compiler away from each other.
    #[test]
    fn forwarder_posts_to_the_log_path() {
        assert!(CONSOLE_FORWARDER.contains(&format!("\"{LOG_PATH}\"")));
        assert!(CONSOLE_FORWARDER.contains(&format!("\"{}\"", crate::run::LOGWISE_PATH)));
    }

    #[test]
    fn decodes_logwise_v1_golden_vector() {
        const HEX: &str = "4c57310001000100c80000002a000000000000000300000000000000090000000000000007000000000000000b000000000000000c000000000000000100150000000000000016000000000000000302000c00676f6c64656e2e6576656e74070066697874757265050067756573740600676f6c64656e0001040063617365010900676f6c64656e2e727311000000040000000300000003000600616374697665000001010500636f756e74010103630000000000000005006c6162656c000005040068c3a96c00";
        let bytes: Vec<u8> = HEX
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(
            summarize_logwise(&bytes).as_deref(),
            Some("logwise[42] golden.event test=case worker=7 context=11 dropped=3 truncated=9")
        );
    }

    /// The shell must contribute no elements, so a program's own DOM lays out
    /// exactly as it will on the page it ships with. A `<pre>` here is what put
    /// the log above a canvas and pushed it down the page as it grew.
    #[test]
    fn index_html_adds_no_elements_to_the_document() {
        let html = index_html(Path::new("demo.wasm"), PROGRAM_JS);
        let body = html.split_once("<body>").expect("shell has a body").1;
        let elements: Vec<&str> = body
            .match_indices('<')
            .map(|(at, _)| body[at..].split(['>', ' ']).next().unwrap_or(""))
            .filter(|tag| !tag.starts_with("</"))
            .collect();
        assert_eq!(elements, ["<script", "<script"], "in: {body}");
    }

    #[test]
    fn decodes_a_batch_into_level_and_text() {
        let batch = "info hello\nerror it broke\n";
        assert_eq!(
            decode_log_batch(batch),
            [
                ("info", "hello".to_string()),
                ("error", "it broke".to_string())
            ]
        );
    }

    /// A panic message carries a stack trace. Unescaped it would read as
    /// several records, each with a stack frame where its level should be.
    #[test]
    fn a_multiline_message_stays_one_record() {
        let batch = "error panicked at lib.rs:1\\n  stack frame\n";
        assert_eq!(
            decode_log_batch(batch),
            [("error", "panicked at lib.rs:1\n  stack frame".to_string())]
        );
    }

    /// Text containing a literal backslash must not be read as an escape —
    /// a Windows path in a log line is the everyday case.
    #[test]
    fn a_literal_backslash_survives_the_round_trip() {
        assert_eq!(
            decode_log_batch("log C:\\\\tmp\\\\n\n"),
            [("log", "C:\\tmp\\n".to_string())]
        );
    }

    /// A batch clipped at `MAX_REQUEST_BODY` ends mid-record. The whole records
    /// before it are still good and must be printed.
    #[test]
    fn a_clipped_final_record_is_dropped_not_mangled() {
        assert_eq!(
            decode_log_batch("log first\nlog second\nlog thi"),
            [("log", "first".to_string()), ("log", "second".to_string())]
        );
    }

    #[test]
    fn a_record_with_no_message_is_not_malformed() {
        assert_eq!(decode_log_batch("log\n"), [("log", String::new())]);
        assert_eq!(decode_log_batch("log \n"), [("log", String::new())]);
    }
}
