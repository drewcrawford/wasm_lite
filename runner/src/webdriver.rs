//! A tiny, dependency-free WebDriver client, with optional browser reuse.
//!
//! Spawns geckodriver / chromedriver / safaridriver and drives the browser over
//! the W3C WebDriver protocol (JSON over HTTP). For un-merged doctests — where
//! the runner is invoked once *per test* — setting `WASM_LITE_REUSE_BROWSER`
//! keeps one session alive across invocations (recorded in a small state file),
//! so N tests share one browser instead of launching N. A lock file serializes
//! concurrent invocations onto the shared session.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Which browser/driver to use. Defaults to Firefox (geckodriver is the most
/// version-tolerant); set `WASM_LITE_BROWSER` to `chrome` or `safari`.
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
            Kind::Safari => r#"{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}"#,
        }
    }
}

/// A WebDriver session, possibly shared across runner invocations.
pub struct Browser {
    /// `Some` when we own the driver (ephemeral): killed on drop.
    driver: Option<Child>,
    /// The driver's port (not the page server's).
    port: u16,
    session: String,
    /// `true` for a reused/persistent session: left alive on drop.
    keep_session: bool,
    /// Held for a persistent session to serialize shared use across processes.
    _lock: Option<Lock>,
}

impl Browser {
    /// Open a browser, reusing a persistent session if `WASM_LITE_REUSE_BROWSER`
    /// is set, otherwise an ephemeral one.
    pub fn open() -> Result<Browser, String> {
        if std::env::var_os("WASM_LITE_REUSE_BROWSER").is_some() {
            Browser::reuse_or_launch()
        } else {
            Browser::launch()
        }
    }

    /// Start a driver + session that close when dropped.
    pub fn launch() -> Result<Browser, String> {
        let kind = Kind::from_env();
        let port = free_port()?;
        let driver = spawn_driver(&kind, port)?;
        wait_ready(port)?;
        let session = new_session(port, kind.capabilities())?;
        Ok(Browser {
            driver: Some(driver),
            port,
            session,
            keep_session: false,
            _lock: None,
        })
    }

    /// Reuse the persistent session if alive, else start a *detached* driver +
    /// session and record it. Holds a lock so concurrent invocations serialize.
    fn reuse_or_launch() -> Result<Browser, String> {
        let lock = Lock::acquire();
        let kind = Kind::from_env();

        if let Some((port, session, _pid)) = read_state()
            && session_alive(port, &session)
        {
            return Ok(Browser {
                driver: None,
                port,
                session,
                keep_session: true,
                _lock: Some(lock),
            });
        }

        let port = free_port()?;
        let driver = spawn_driver(&kind, port)?;
        let pid = driver.id();
        // Detach: dropping the Child does not kill it, so the driver outlives
        // this runner process and can be reused by the next one.
        drop(driver);
        wait_ready(port)?;
        let session = new_session(port, kind.capabilities())?;
        write_state(port, &session, pid);
        Ok(Browser {
            driver: None,
            port,
            session,
            keep_session: true,
            _lock: Some(lock),
        })
    }

    /// Close the persistent session and kill its driver (used by `--stop-browser`).
    pub fn stop_persistent() {
        if let Some((port, session, pid)) = read_state() {
            let _ = http(port, "DELETE", &format!("/session/{session}"), None);
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        clear_state();
    }

    /// Navigate the session to `url`.
    pub fn goto(&self, url: &str) -> Result<(), String> {
        let body = format!("{{\"url\":{}}}", json_str(url));
        http(
            self.port,
            "POST",
            &format!("/session/{}/url", self.session),
            Some(&body),
        )?;
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
        http(
            self.port,
            "POST",
            &format!("/session/{}/execute/sync", self.session),
            Some(&body),
        )
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if self.keep_session {
            return; // persistent: leave the session + driver alive for reuse
        }
        let _ = http(
            self.port,
            "DELETE",
            &format!("/session/{}", self.session),
            None,
        );
        if let Some(driver) = self.driver.as_mut() {
            let _ = driver.kill();
            let _ = driver.wait();
        }
    }
}

fn spawn_driver(kind: &Kind, port: u16) -> Result<Child, String> {
    Command::new(kind.driver())
        .arg(format!("--port={port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {} (is it installed?): {e}", kind.driver()))
}

fn wait_ready(port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(body) = http(port, "GET", "/status", None)
            && body.contains("\"ready\":true")
        {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("WebDriver did not become ready".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn new_session(port: u16, capabilities: &str) -> Result<String, String> {
    let resp = http(port, "POST", "/session", Some(capabilities))?;
    json_string_after_key(&resp, "sessionId").ok_or_else(|| format!("session not created: {resp}"))
}

/// Is `session` still usable on `port`?
fn session_alive(port: u16, session: &str) -> bool {
    match http(port, "GET", &format!("/session/{session}/url"), None) {
        Ok(body) => !body.contains("\"error\""),
        Err(_) => false,
    }
}

/// One-shot HTTP/1.1 request to a WebDriver; returns the response body.
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connect driver: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;

    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n{body}",
        body.len(),
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    read_http_body(&mut stream)
}

/// Read an HTTP/1.1 response and return its body (honors `Content-Length`).
fn read_http_body(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(header_end) = find(&buf, b"\r\n\r\n") {
            let body_start = header_end + 4;
            match content_length(&buf[..header_end]) {
                Some(len) if buf.len() >= body_start + len => {
                    return Ok(
                        String::from_utf8_lossy(&buf[body_start..body_start + len]).into_owned()
                    );
                }
                Some(_) => {}
                None => return Ok(String::from_utf8_lossy(&buf[body_start..]).into_owned()),
            }
        }
        let n = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            let body = find(&buf, b"\r\n\r\n").map(|h| h + 4).unwrap_or(buf.len());
            return Ok(String::from_utf8_lossy(&buf[body..]).into_owned());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(headers).ok()?;
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn free_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not find a free port: {e}"))?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

// --- shared-browser state + lock --------------------------------------------

fn state_path() -> PathBuf {
    std::env::temp_dir().join("wasm_lite_browser.state")
}

/// `(driver port, session id, driver pid)`.
fn read_state() -> Option<(u16, String, u32)> {
    let text = fs::read_to_string(state_path()).ok()?;
    let mut lines = text.lines();
    let port = lines.next()?.parse().ok()?;
    let session = lines.next()?.to_string();
    let pid = lines.next()?.parse().ok()?;
    Some((port, session, pid))
}

fn write_state(port: u16, session: &str, pid: u32) {
    let _ = fs::write(state_path(), format!("{port}\n{session}\n{pid}\n"));
}

fn clear_state() {
    let _ = fs::remove_file(state_path());
}

/// A cross-process lock (an exclusively-created file) serializing use of the
/// shared browser session. Released on drop; a stale lock is stolen after 2 min.
struct Lock(PathBuf);

impl Lock {
    fn acquire() -> Lock {
        let path = std::env::temp_dir().join("wasm_lite_browser.lock");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Lock(path),
                Err(_) => {
                    if Instant::now() > deadline {
                        let _ = fs::remove_file(&path); // steal a stale lock
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

// --- minimal JSON ------------------------------------------------------------

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

fn value_is_true(resp: &str) -> bool {
    match resp.find("\"value\"") {
        Some(idx) => resp[idx + 7..]
            .trim_start_matches([':', ' '])
            .starts_with("true"),
        None => false,
    }
}

fn value_string(resp: &str) -> Option<String> {
    json_string_after_key(resp, "value")
}

fn json_string_after_key(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = s.find(&needle)? + needle.len();
    let after = s[start..].trim_start().strip_prefix(':')?.trim_start();
    parse_json_string(after)
}

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
