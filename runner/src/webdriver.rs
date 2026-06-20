//! A tiny, dependency-free WebDriver client.
//!
//! Spawns `chromedriver`, opens a headless Chrome session, and drives it over
//! the W3C WebDriver protocol (JSON over HTTP). Just enough to navigate to a
//! page and poll the result of a script — no general JSON library, only the
//! small bits of response parsing we need.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Which browser/driver to use. Defaults to Firefox (geckodriver is the most
/// version-tolerant); set `WASM_LITE_BROWSER` to `chrome` or `safari`.
///
/// Note: Safari has no headless mode, so it opens a real window during the run
/// (and requires "Allow Remote Automation" — `safaridriver --enable`).
enum Kind {
    Firefox,
    Chrome,
    Safari,
}

impl Kind {
    fn from_env() -> Kind {
        match std::env::var("WASM_LITE_BROWSER").as_deref() {
            Ok("chrome") | Ok("chromium") => Kind::Chrome,
            Ok("safari") => Kind::Safari,
            _ => Kind::Firefox,
        }
    }

    fn driver(&self) -> &'static str {
        match self {
            Kind::Firefox => "geckodriver",
            Kind::Chrome => "chromedriver",
            Kind::Safari => "safaridriver",
        }
    }

    fn capabilities(&self) -> &'static str {
        match self {
            Kind::Firefox => {
                r#"{"capabilities":{"alwaysMatch":{"browserName":"firefox","moz:firefoxOptions":{"args":["-headless"]}}}}"#
            }
            Kind::Chrome => {
                r#"{"capabilities":{"alwaysMatch":{"browserName":"chrome","goog:chromeOptions":{"args":["--headless=new","--disable-gpu","--no-sandbox","--disable-dev-shm-usage"]}}}}"#
            }
            // Safari has no headless mode and no extra options.
            Kind::Safari => r#"{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}"#,
        }
    }
}

/// A headless browser session backed by a WebDriver child process.
pub struct Browser {
    driver: Child,
    port: u16,
    session: String,
}

impl Browser {
    /// Start the WebDriver and open a headless browser session.
    pub fn launch() -> Result<Browser, String> {
        let kind = Kind::from_env();
        let port = free_port()?;
        let driver = Command::new(kind.driver())
            .arg(format!("--port={port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {} (is it installed?): {e}", kind.driver()))?;

        let mut browser = Browser {
            driver,
            port,
            session: String::new(),
        };
        browser.wait_ready()?;
        browser.session = browser.new_session(kind.capabilities())?;
        Ok(browser)
    }

    /// Navigate the session to `url`.
    pub fn goto(&self, url: &str) -> Result<(), String> {
        let body = format!("{{\"url\":{}}}", json_str(url));
        self.http("POST", &format!("/session/{}/url", self.session), Some(&body))?;
        Ok(())
    }

    /// Run `script` and return whether it evaluated to `true`.
    pub fn eval_bool(&self, script: &str) -> Result<bool, String> {
        Ok(value_is_true(&self.execute(script)?))
    }

    /// Run `script` and return its string result (empty if not a string).
    pub fn eval_string(&self, script: &str) -> Result<String, String> {
        Ok(value_string(&self.execute(script)?).unwrap_or_default())
    }

    fn execute(&self, script: &str) -> Result<String, String> {
        let body = format!("{{\"script\":{},\"args\":[]}}", json_str(script));
        self.http("POST", &format!("/session/{}/execute/sync", self.session), Some(&body))
    }

    fn new_session(&self, capabilities: &str) -> Result<String, String> {
        let resp = self.http("POST", "/session", Some(capabilities))?;
        json_string_after_key(&resp, "sessionId")
            .ok_or_else(|| format!("session not created: {resp}"))
    }

    fn wait_ready(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(body) = self.http("GET", "/status", None) {
                if body.contains("\"ready\":true") {
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                return Err("chromedriver did not become ready".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// One-shot HTTP/1.1 request to chromedriver; returns the response body.
    fn http(&self, method: &str, path: &str, body: Option<&str>) -> Result<String, String> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .map_err(|e| format!("connect chromedriver: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;

        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{}\r\n\
             Content-Type: application/json; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n{body}",
            self.port,
            body.len(),
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {e}"))?;

        read_http_body(&mut stream)
    }
}

/// Read an HTTP/1.1 response and return its body. Honors `Content-Length`;
/// falls back to reading until the connection closes. (chromedriver keeps the
/// connection alive, so we cannot rely on EOF alone.)
fn read_http_body(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(header_end) = find(&buf, b"\r\n\r\n") {
            let body_start = header_end + 4;
            match content_length(&buf[..header_end]) {
                Some(len) if buf.len() >= body_start + len => {
                    return Ok(String::from_utf8_lossy(&buf[body_start..body_start + len]).into_owned());
                }
                Some(_) => {} // need more body bytes
                None => return Ok(String::from_utf8_lossy(&buf[body_start..]).into_owned()),
            }
        }
        let n = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            // Connection closed: return whatever body we have.
            let body = find(&buf, b"\r\n\r\n").map(|h| h + 4).unwrap_or(buf.len());
            return Ok(String::from_utf8_lossy(&buf[body..]).into_owned());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Parse the `Content-Length` value from raw response headers (case-insensitive).
fn content_length(headers: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(headers).ok()?;
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
}

impl Drop for Browser {
    fn drop(&mut self) {
        if !self.session.is_empty() {
            let _ = self.http("DELETE", &format!("/session/{}", self.session), None);
        }
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}

/// Grab a free localhost port (chromedriver binds it next; small race window).
fn free_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not find a free port: {e}"))?;
    Ok(listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port())
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Is the top-level `"value"` of a WebDriver response `true`?
fn value_is_true(resp: &str) -> bool {
    match resp.find("\"value\"") {
        Some(idx) => resp[idx + 7..]
            .trim_start_matches([':', ' '])
            .starts_with("true"),
        None => false,
    }
}

/// Extract the top-level `"value"` of a WebDriver response as a string.
fn value_string(resp: &str) -> Option<String> {
    json_string_after_key(resp, "value")
}

/// Find `"key"` and parse the JSON string that follows the colon.
fn json_string_after_key(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = s.find(&needle)? + needle.len();
    let after = s[start..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    parse_json_string(after)
}

/// Parse a JSON string literal at the start of `s`, decoding escapes.
fn parse_json_string(s: &str) -> Option<String> {
    let mut chars = s.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    let cp = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                }
                _ => return None,
            },
            other => out.push(other),
        }
    }
    None
}
