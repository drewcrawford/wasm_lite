// SPDX-License-Identifier: MIT OR Apache-2.0
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
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

/// What a signal handler needs in order to close a session it does not own.
///
/// The driver is identified by pid rather than by [`Child`], because the
/// cleanup runs on a different thread from the one holding the handle.
struct Cleanup {
    port: u16,
    /// Empty until the session exists: a driver is worth killing before then.
    session: String,
    driver_pid: u32,
}

/// The ephemeral session to close if the runner is signalled, if any.
static CLEANUP: Mutex<Option<Cleanup>> = Mutex::new(None);

/// Guards one-time setup of the signal handler and its watchdog thread.
static ARMED: Once = Once::new();

/// Record what to close if a signal arrives, replacing any earlier record.
fn register_cleanup(cleanup: Cleanup) {
    *CLEANUP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cleanup);
}

/// Take the registered session, if there is one.
fn take_cleanup() -> Option<Cleanup> {
    CLEANUP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Arrange for a signalled runner to close its browser instead of orphaning it.
///
/// `Drop` covers every ordinary exit, including a failed test; it does not
/// cover `SIGTERM`, which is how a CI job or a killed shell ends the runner.
/// Without this, the driver and browser survive with the test page still
/// live — and a page that was spin-waiting keeps a core pinned indefinitely.
///
/// Called before the driver is spawned so that a signal arriving *during*
/// startup still terminates the process promptly.
fn arm() {
    ARMED.call_once(|| {
        super::signals::install();
        std::thread::spawn(|| {
            loop {
                if let Some(sig) = super::signals::pending() {
                    if let Some(cleanup) = take_cleanup() {
                        teardown(cleanup.port, &cleanup.session, Some(cleanup.driver_pid));
                    }
                    std::process::exit(128 + sig);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
    });
}

/// Close a session and kill its driver.
///
/// The `DELETE` runs on its own thread against a deadline: a wedged driver
/// must not be able to stall a Ctrl-C, and killing it is what actually stops
/// the browser, so that step has to be reachable even when the graceful close
/// never answers.
fn teardown(port: u16, session: &str, driver_pid: Option<u32>) {
    if !session.is_empty() {
        let (tx, rx) = std::sync::mpsc::channel();
        let session = session.to_string();
        std::thread::spawn(move || {
            let _ = http(port, "DELETE", &format!("/session/{session}"), None);
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
    if let Some(pid) = driver_pid {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

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

    fn capabilities(&self) -> String {
        let extra = extra_args();
        match self {
            Kind::Firefox => {
                let args = join_args(&["-headless"], &extra);
                format!(
                    r#"{{"capabilities":{{"alwaysMatch":{{"browserName":"firefox","moz:firefoxOptions":{{"args":[{args}]}}}}}}}}"#
                )
            }
            Kind::Chrome => {
                // `--disable-gpu` is right for a suite that only touches the
                // DOM and wrong for one that touches WebGPU: with it,
                // `navigator.gpu.requestAdapter()` resolves to null and every
                // graphics test fails for a reason that looks like a bug in the
                // code under test. `WASM_LITE_GPU=1` swaps it for SwiftShader,
                // which gives headless Chrome a real (software) adapter.
                let base: &[&str] = if gpu_requested() {
                    &[
                        "--headless=new",
                        "--enable-unsafe-swiftshader",
                        "--no-sandbox",
                        "--disable-dev-shm-usage",
                    ]
                } else {
                    &[
                        "--headless=new",
                        "--disable-gpu",
                        "--no-sandbox",
                        "--disable-dev-shm-usage",
                    ]
                };
                let args = join_args(base, &extra);
                format!(
                    r#"{{"capabilities":{{"alwaysMatch":{{"browserName":"chrome","goog:chromeOptions":{{"args":[{args}]}}}}}}}}"#
                )
            }
            Kind::Safari => {
                r#"{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}"#.to_string()
            }
        }
    }
}

/// Did the caller ask for a GPU-capable browser (`WASM_LITE_GPU`)?
fn gpu_requested() -> bool {
    std::env::var_os("WASM_LITE_GPU").is_some()
}

/// Extra browser arguments from `WASM_LITE_BROWSER_ARGS`, split on whitespace.
///
/// An escape hatch for the flag this runner has not thought of — a particular
/// ANGLE backend, a feature flag, a device-scale factor.
fn extra_args() -> Vec<String> {
    std::env::var("WASM_LITE_BROWSER_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Render arguments as a JSON array body (without the brackets).
///
/// Arguments are browser flags, so they contain no quotes or backslashes to
/// escape; anything that does is rejected rather than emitted, since a
/// half-escaped string would corrupt the whole capabilities document.
fn join_args(base: &[&str], extra: &[String]) -> String {
    base.iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
        .filter(|a| {
            let clean = !a.contains('"') && !a.contains('\\');
            if !clean {
                eprintln!("warning: ignoring browser argument with a quote or backslash: {a}");
            }
            clean
        })
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(",")
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
    /// Held for an ephemeral session: one slot in the host-wide browser cap.
    /// A reused session is a single browser shared by all comers, so it needs
    /// no slot — `_lock` already serializes it.
    _admission: Option<Admission>,
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
        arm();
        // Before the driver, not after: the whole point is to not be the
        // process that starts an eighth browser on a host with room for four.
        let admission = Admission::acquire();
        let kind = Kind::from_env();
        let port = free_port()?;
        let mut driver = spawn_driver(&kind, port)?;
        // Register before the session exists. Session setup takes seconds —
        // long enough to be signalled in — and a driver killed at that point
        // would otherwise outlive the runner with a browser already attached.
        register_cleanup(Cleanup {
            port,
            session: String::new(),
            driver_pid: driver.id(),
        });
        // Kill the driver if session setup fails: dropping a Child does not
        // terminate it, so bailing here would leak a driver process (holding
        // its port) on every failed attempt.
        let session = wait_ready(port)
            .and_then(|()| new_session(port, &kind.capabilities()))
            .inspect_err(|_| {
                let _ = take_cleanup();
                let _ = driver.kill();
                let _ = driver.wait();
            })?;
        register_cleanup(Cleanup {
            port,
            session: session.clone(),
            driver_pid: driver.id(),
        });
        Ok(Browser {
            driver: Some(driver),
            port,
            session,
            keep_session: false,
            _lock: None,
            _admission: Some(admission),
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
                _admission: None,
            });
        }

        let port = free_port()?;
        let mut driver = spawn_driver(&kind, port)?;
        let pid = driver.id();
        // On setup failure, kill the driver rather than detaching it: no state
        // file is written on this path, so `--stop-browser` could never find
        // the orphan.
        let session = wait_ready(port)
            .and_then(|()| new_session(port, &kind.capabilities()))
            .inspect_err(|_| {
                let _ = driver.kill();
                let _ = driver.wait();
            })?;
        // Detach: dropping the Child does not kill it, so the driver outlives
        // this runner process and can be reused by the next one.
        drop(driver);
        write_state(port, &session, pid);
        Ok(Browser {
            driver: None,
            port,
            session,
            keep_session: true,
            _lock: Some(lock),
            _admission: None,
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
        // Every script doubles as the heartbeat the page's watchdog waits on:
        // the runner polls constantly while it is alive, so "no script for a
        // long time" means it is not. Stamping it here rather than at each
        // call site means a new kind of poll cannot forget to do it.
        let script = format!("globalThis.__wl_hb = Date.now();\n{script}");
        let body = format!("{{\"script\":{},\"args\":[]}}", json_str(&script));
        let resp = http(
            self.port,
            "POST",
            &format!("/session/{}/execute/sync", self.session),
            Some(&body),
        )?;
        // Surface WebDriver-level failures (dead session, JS exception, …):
        // otherwise the error payload reads as `false`/empty to the eval_*
        // helpers and the caller polls to a misleading timeout instead of
        // reporting the real cause. Both geckodriver and chromedriver emit
        // errors as compact `{"value":{"error":...}}`.
        if resp.contains("{\"value\":{\"error\":") {
            let err = json_string_after_key(&resp, "error").unwrap_or_default();
            let msg = json_string_after_key(&resp, "message").unwrap_or_default();
            return Err(format!("WebDriver error: {err}: {msg}"));
        }
        Ok(resp)
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Deregister first, so a signal arriving mid-teardown does not send the
        // watchdog after a session this thread is already closing.
        let _ = take_cleanup();
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
                // No Content-Length: the request sent `Connection: close`, so
                // keep reading to EOF (the n == 0 arm below) rather than
                // returning whatever happens to be buffered — the body may
                // arrive in later packets.
                None => {}
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

// --- concurrent-browser admission -------------------------------------------

/// Roughly what one browser + driver + loaded page costs, in MiB.
///
/// Deliberately whole-browser rather than per-tab: each admitted runner starts
/// its own browser, and a browser that gets its content processes OOM-killed
/// reports as `no such window: Browsing context has been discarded` rather than
/// as memory pressure, so erring small here buys nothing.
const MIB_PER_BROWSER: u64 = 1024;

/// Cap when the host's free memory cannot be read: enough to keep a laptop
/// busy, low enough not to melt a small CI box.
const DEFAULT_MAX_BROWSERS: usize = 2;

/// How many browsers this host may run at once.
///
/// `cargo test --doc` invokes the runner once per doctest and runs those in
/// parallel across every core, so without a cap an 8-core, 8 GB machine
/// launches 8 browsers and fails tests that pass serially — as `os error 11`
/// on a thread the browser could not spawn, or as a discarded browsing context.
/// Since the limit is memory, not CPU, derive it from free memory and only then
/// clamp to the core count.
fn max_browsers() -> usize {
    if let Some(n) = std::env::var("WASM_LITE_MAX_BROWSERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return n.max(1);
    }
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    match available_mib() {
        Some(mib) => ((mib / MIB_PER_BROWSER) as usize).clamp(1, cpus),
        None => DEFAULT_MAX_BROWSERS.min(cpus),
    }
}

/// Memory available for new processes, in MiB.
///
/// `MemAvailable` rather than `MemFree`: reclaimable page cache is usable, and
/// `MemFree` alone would under-report badly on any box that has run a build.
/// `None` anywhere without procfs, which the caller treats as "unknown".
fn available_mib() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let line = text
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))?
        .strip_prefix("MemAvailable:")?;
    // "MemAvailable:   4615141 kB"
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kib / 1024)
}

/// A held slot in the cross-process browser semaphore. Released on drop.
struct Admission(Option<PathBuf>);

/// How long a slot file may go untouched before a waiter reclaims it. Only
/// reached when the pid check cannot settle it (a non-Linux host, or a pid the
/// OS has since recycled).
const SLOT_STALE_AFTER: Duration = Duration::from_secs(900);

impl Admission {
    /// Block until one of `max_browsers()` slots is free, then claim it.
    ///
    /// Slots are files in a shared directory, claimed by exclusive create — the
    /// same trick as [`Lock`], but N-deep instead of 1. Waiting here is the
    /// point: the caller is a runner process that would otherwise add another
    /// browser to an already-thrashing host.
    fn acquire() -> Admission {
        let dir = std::env::temp_dir().join(format!("wasm_lite_browsers_{}", user_suffix()));
        Admission::acquire_in(&dir, max_browsers())
    }

    /// [`Admission::acquire`] against an explicit directory and limit.
    fn acquire_in(dir: &std::path::Path, limit: usize) -> Admission {
        if fs::create_dir_all(dir).is_err() {
            return Admission(None); // no temp dir to coordinate through: proceed uncapped
        }
        let mut announced = false;
        loop {
            if let Some(admission) = Admission::try_acquire_in(dir, limit) {
                return admission;
            }
            if !announced {
                announced = true;
                eprintln!("wasm_lite: waiting for a browser slot ({limit} in use)");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Claim a slot if one is free right now, else `None`.
    fn try_acquire_in(dir: &std::path::Path, limit: usize) -> Option<Admission> {
        fs::create_dir_all(dir).ok()?;
        (0..limit)
            .find_map(|slot| Admission::claim(&dir.join(format!("slot{slot}"))))
            .map(|path| Admission(Some(path)))
    }

    /// Take one specific slot, reclaiming it first if its holder is gone.
    fn claim(path: &std::path::Path) -> Option<PathBuf> {
        let write_pid = |mut file: fs::File| {
            let _ = file.write_all(std::process::id().to_string().as_bytes());
            Some(path.to_path_buf())
        };
        let create = || OpenOptions::new().write(true).create_new(true).open(path);
        if let Ok(file) = create() {
            return write_pid(file);
        }
        // Retry the same slot after reclaiming it: skipping to the next one
        // would report a full pool while holding a slot nobody owns.
        if slot_is_dead(path) {
            let _ = fs::remove_file(path);
            if let Ok(file) = create() {
                return write_pid(file);
            }
        }
        None
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            // Only reclaim a slot still recorded as ours: if a waiter judged it
            // dead and took it, deleting would evict that innocent holder.
            if fs::read_to_string(path).is_ok_and(|pid| pid == std::process::id().to_string()) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Whether a slot's holder is gone, so the slot can be reclaimed.
///
/// A runner killed with `SIGKILL` runs no destructor, so slots do leak; left
/// alone they would permanently shrink the pool. Prefer the pid check — it is
/// exact and immediate — and fall back to file age where procfs is absent.
fn slot_is_dead(path: &std::path::Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    if let Ok(pid) = contents.trim().parse::<u32>()
        && cfg!(target_os = "linux")
        && PathBuf::from("/proc").is_dir()
    {
        return !PathBuf::from(format!("/proc/{pid}")).exists();
    }
    fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > SLOT_STALE_AFTER))
}

// --- shared-browser state + lock --------------------------------------------

/// A per-user filename component: the temp dir may be world-writable and
/// shared (`/tmp` on Linux), so fixed names would let unrelated users collide
/// with — or squat on — each other's state and lock files.
fn user_suffix() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "shared".to_string())
}

fn state_path() -> PathBuf {
    std::env::temp_dir().join(format!("wasm_lite_browser_{}.state", user_suffix()))
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

/// A cross-process lock (an exclusively-created file holding the owner's PID)
/// serializing use of the shared browser session. Released on drop; a lock
/// whose *file* is old enough is presumed leaked by a crashed holder and stolen.
struct Lock(PathBuf);

/// How old a lock file must be before a waiter may steal it. Generous: a
/// legitimate holder can be a whole browser test suite, and stealing a live
/// lock means two processes driving one browser session.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(600);

impl Lock {
    fn acquire() -> Lock {
        let path = std::env::temp_dir().join(format!("wasm_lite_browser_{}.lock", user_suffix()));
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = file.write_all(std::process::id().to_string().as_bytes());
                    return Lock(path);
                }
                Err(_) => {
                    // Steal only a genuinely stale lock, judged by the lock
                    // file's age — not by how long *this* waiter has been
                    // waiting, which would let it delete a lock some third
                    // process had only just acquired.
                    if let Ok(meta) = fs::metadata(&path)
                        && let Ok(modified) = meta.modified()
                        && modified.elapsed().is_ok_and(|age| age > LOCK_STALE_AFTER)
                    {
                        let _ = fs::remove_file(&path);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Remove only a lock we still own: if ours was stolen as stale and
        // another process re-acquired, blindly deleting would cascade the
        // theft to a third process.
        if fs::read_to_string(&self.0).is_ok_and(|pid| pid == std::process::id().to_string()) {
            let _ = fs::remove_file(&self.0);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of our own, so concurrent tests never share slots.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wl_admit_test_{}_{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn admission_caps_concurrent_browsers() {
        let dir = scratch("cap");
        let _first = Admission::acquire_in(&dir, 2);
        let _second = Admission::acquire_in(&dir, 2);
        // Both slots are taken; a third caller must wait rather than launch.
        assert!(Admission::try_acquire_in(&dir, 2).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_an_admission_frees_its_slot() {
        let dir = scratch("free");
        let first = Admission::acquire_in(&dir, 1);
        assert!(Admission::try_acquire_in(&dir, 1).is_none());
        drop(first);
        assert!(Admission::try_acquire_in(&dir, 1).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slot_left_by_a_dead_holder_is_reclaimed() {
        let dir = scratch("stale");
        fs::create_dir_all(&dir).unwrap();
        // A pid that cannot be running: a SIGKILLed runner leaves exactly this,
        // and without reclamation the pool would shrink by one for good.
        fs::write(dir.join("slot0"), "4294967295").unwrap();
        assert!(Admission::try_acquire_in(&dir, 1).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slot_held_by_a_live_holder_is_kept() {
        let dir = scratch("live");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("slot0"), std::process::id().to_string()).unwrap();
        assert!(Admission::try_acquire_in(&dir, 1).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn browser_limit_stays_within_one_and_the_core_count() {
        // A memory-starved host must still make progress, one browser at a
        // time; a roomy one must not exceed the cores it can actually drive.
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let limit = max_browsers();
        assert!((1..=cpus).contains(&limit), "limit {limit} for {cpus} cpus");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn available_memory_is_readable_and_sane() {
        // The whole cap rests on this parse; a silent `None` would quietly
        // demote every host to DEFAULT_MAX_BROWSERS.
        let mib = available_mib().expect("MemAvailable on a Linux host");
        assert!(mib > 0 && mib < 1024 * 1024, "{mib} MiB");
    }
}
