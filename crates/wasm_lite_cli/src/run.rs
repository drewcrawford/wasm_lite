// SPDX-License-Identifier: MIT OR Apache-2.0
//! wasm_lite runner.
//!
//! Serves a single program over a local HTTP server and opens it in the
//! system default browser. The program may be:
//!
//!   * a `.js` file — served as-is and loaded as an ES module, or
//!   * a `.wasm` file — served as `application/wasm` alongside a generated
//!     loader that supplies the host imports and calls the module's `main`.
//!
//! The runner is intentionally dependency-free and built on `std` only.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
mod shell;
mod signals;
mod test_runner;
mod webdriver;
mod ws_echo;

/// Path under which the HTML shell loads the program (an ES module).
const PROGRAM_JS: &str = "/program.js";
/// Path under which a wasm module is served.
const PROGRAM_WASM: &str = "/program.wasm";
/// Bootstrap module: imports the glue and runs the program's `main`. Kept
/// separate from the glue so a spawned worker can import the glue without
/// re-running `main`.
const BOOTSTRAP_JS: &str = "/bootstrap.js";
/// Worker bootstrap module for spawned threads (shared-memory builds only).
const WL_WORKER_JS: &str = "/wl_worker.js";
/// Interop only: the wasm_lite-generated glue module.
const WL_GLUE_JS: &str = "/wl_glue.js";
/// Interop only: the (patched) wasm-bindgen-generated glue module.
const WB_GLUE_JS: &str = "/wb_glue.js";
/// Endpoint the interactive shell POSTs batched console output to. Not a
/// [`Route`]: it has no body to serve, and is answered before the route table.
const LOG_PATH: &str = "/__wl_log";

/// Ceiling on a request body, so a runaway or hostile client cannot make the
/// runner allocate without bound.
///
/// Only [`LOG_PATH`] has a body at all, and 512 console lines — the forwarder's
/// own flush threshold — do not come close. A batch over this is pathological,
/// and is clipped rather than refused: [`decode_log_batch`] ignores the
/// resulting partial record, so the cost is the tail of one line.
const MAX_REQUEST_BODY: u64 = 1 << 20;

/// A single static resource served by the runner.
struct Route {
    path: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

pub fn main(argv: Vec<OsString>) {
    // Stop a persistent (reused) browser started via `WASM_LITE_REUSE_BROWSER`.
    if argv.iter().any(|a| a == "--stop-browser") {
        webdriver::Browser::stop_persistent();
        println!("stopped the persistent browser");
        return;
    }

    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("usage: wasm_lite run [--serve] <program.js|program.wasm>");
            std::process::exit(2);
        }
    };

    // Run headless and exit with a status code for `cargo test` and doctests;
    // serve + open a browser for `cargo run` and direct/interactive use. The two
    // are told apart by how Cargo invokes us (see `is_test_run`).
    if is_test_run(&args) {
        std::process::exit(test_runner::run(&args));
    }

    serve_interactive(&args.program);
}

/// Should we run headless (and exit) rather than serve interactively?
///
/// Cargo gives no explicit signal, so we infer the context from the artifact:
/// `cargo test` binaries live under `deps/`, and rustdoc doctests in a
/// `rustdoctest…` temp dir — both want headless + exit. A `#[wasm_lite_test]`
/// harness is conclusive. Everything else (`cargo run`, direct invocation) is
/// treated as interactive and served.
fn is_test_run(args: &Args) -> bool {
    if args.serve {
        return false;
    }
    if args.test || args.list || args.bench {
        return true;
    }

    // `WASM_LITE_RUN_SECONDS` means "watch this program for N seconds and print
    // what it logged", which is only the headless path — the interactive server
    // never exits and never prints. Honour it as a mode selector, so
    // `WASM_LITE_RUN_SECONDS=60 cargo run --target wasm32-unknown-unknown` does
    // what its documentation says instead of silently serving forever.
    if std::env::var_os("WASM_LITE_RUN_SECONDS").is_some() {
        return true;
    }

    // Cargo puts test artifacts directly in `target/…/deps/`, while `cargo run`
    // bins live in `target/…/debug/`. Check the immediate parent (not a
    // substring): a project checked out under a directory literally named
    // `deps` must not force headless mode, and `Path` components also handle
    // Windows separators, which `contains("/deps/")` did not.
    if args
        .program
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "deps")
        || args.program.to_string_lossy().contains("rustdoctest")
        || in_build_out_dir(&args.program)
    {
        return true;
    }

    // A parse error here just means "not conclusively a test harness"; the
    // headless path re-reads the module and reports the real error.
    std::fs::read(&args.program)
        .map(|module| {
            wasm_lite_codegen::test_decls(&module).is_ok_and(|decls| !decls.is_empty())
                || wasm_lite_codegen::bench_decls(&module).is_ok_and(|decls| !decls.is_empty())
        })
        .unwrap_or(false)
}

/// Is the artifact in a cargo build directory (`…/build/<pkg>/<hash>/out/`)?
///
/// `deps/` is not the only place a test binary lands. Under `-Z build-std`,
/// cargo 1.99-nightly emitted one here instead — which older cargo, and so this
/// runner, had never seen. Misreading a test as a `cargo run` is not a
/// mislabelled failure but a hang, because the interactive path serves
/// forever, so it is worth recognizing both layouts.
fn in_build_out_dir(path: &Path) -> bool {
    path.parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "out")
        && path.components().any(|c| c.as_os_str() == "build")
}

/// Serve the program and open a browser; runs until interrupted.
fn serve_interactive(program: &Path) -> ! {
    let routes = match build_routes(program) {
        Ok(routes) => routes,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    let listener = bind().unwrap_or_else(|err| {
        eprintln!("error: could not bind a local port: {err}");
        std::process::exit(1);
    });
    let addr = listener.local_addr().expect("listener has a local address");
    let url = format!("http://{addr}/");

    println!("serving on {url}");
    // WASM_LITE_NO_OPEN keeps the server up without launching a browser (e.g.
    // when an external automated browser will connect).
    if std::env::var_os("WASM_LITE_NO_OPEN").is_none() {
        // Serving forever is the point interactively, and useless anywhere a
        // browser cannot be opened: on a headless CI box it turns a runner that
        // should have failed into a job that hangs until its timeout. Refuse
        // rather than wait for a viewer that is never going to arrive; someone
        // who means to attach one themselves says so with WASM_LITE_NO_OPEN.
        if let Err(err) = open_browser(&url) {
            eprintln!("error: could not open a browser: {err}");
            eprintln!("set WASM_LITE_NO_OPEN=1 to serve anyway and connect one yourself");
            std::process::exit(1);
        }
        println!("opening browser... (ctrl-c to stop)");
    }

    serve(listener, &routes);
}

/// Parsed command-line arguments.
struct Args {
    program: PathBuf,
    serve: bool,
    test: bool,
    /// libtest-style name filters (positional args after the program).
    filters: Vec<String>,
    /// `--exact`: filters match whole names rather than substrings.
    exact: bool,
    /// `--list`: print the (filtered) test names and exit, like libtest.
    list: bool,
    /// `--bench`: measure benchmarks rather than just running them once.
    ///
    /// Cargo passes this to a `harness = false` bench target under
    /// `cargo bench`, and omits it under `cargo test` — which is exactly the
    /// distinction we want, so there is nothing to infer.
    bench: bool,
    /// `--include-ignored`: run `#[ignore]`d cases alongside the rest.
    include_ignored: bool,
    /// `--ignored`: run *only* the `#[ignore]`d cases, as libtest does.
    only_ignored: bool,
}

impl Args {
    /// Does an `#[ignore]` flag admit this case, following libtest: ignored
    /// cases are skipped by default, run alongside the rest under
    /// `--include-ignored`, and run to the exclusion of the rest under
    /// `--ignored`.
    fn admits_ignored(&self, ignored: bool) -> bool {
        if self.only_ignored {
            ignored
        } else {
            !ignored || self.include_ignored
        }
    }

    /// Does a test name pass the libtest-style filters?
    fn selects(&self, name: &str) -> bool {
        self.filters.is_empty()
            || self.filters.iter().any(|f| {
                if self.exact {
                    name == f
                } else {
                    name.contains(f.as_str())
                }
            })
    }
}

/// Parse command-line arguments.
///
/// The first non-flag argument is the program (`.js` or `.wasm`); later
/// positional arguments are libtest-style test-name filters (`cargo test foo`
/// invokes us as `runner <artifact.wasm> foo`). `--serve` forces the
/// interactive server; `--test` forces headless test mode; `--bench` asks a
/// bench target for measurements (Cargo passes it under `cargo bench` and not
/// under `cargo test`); `--exact` and `--list` follow libtest. Other flags (e.g. `--nocapture`) are ignored, so
/// the runner works directly as a Cargo runner
/// (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER`).
fn parse_args(argv: Vec<OsString>) -> Result<Args, String> {
    let mut program = None;
    let mut filters = Vec::new();
    let mut serve = false;
    let mut test = false;
    let mut exact = false;
    let mut list = false;
    let mut bench = false;
    let mut include_ignored = false;
    let mut only_ignored = false;
    for arg in argv {
        let text = arg.to_string_lossy();
        if text == "--serve" {
            serve = true;
        } else if text == "--test" {
            test = true;
        } else if text == "--exact" {
            exact = true;
        } else if text == "--list" {
            list = true;
        } else if text == "--bench" {
            bench = true;
        } else if text == "--include-ignored" {
            include_ignored = true;
        } else if text == "--ignored" {
            only_ignored = true;
        } else if text.starts_with('-') {
            // Ignore other flags (e.g. test-harness arguments).
        } else if program.is_none() {
            program = Some(PathBuf::from(arg));
        } else {
            filters.push(text.into_owned());
        }
    }
    Ok(Args {
        program: program.ok_or_else(|| "error: missing program path".to_string())?,
        serve,
        test,
        filters,
        exact,
        list,
        bench,
        include_ignored,
        only_ignored,
    })
}

/// Build the route table for the given program, dispatching on its extension.
fn build_routes(program: &Path) -> Result<Vec<Route>, String> {
    let ext = program.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut routes = Vec::new();

    // Each branch serves the program's modules and returns the entry module the
    // HTML shell should load.
    let entry: &'static str = match ext {
        "js" => {
            let source = read(program)?;
            routes.push(Route {
                path: PROGRAM_JS,
                content_type: "text/javascript; charset=utf-8",
                body: source,
            });
            PROGRAM_JS
        }
        "wasm" => {
            let module = read(program)?;
            // Before the interop branch: an interop module needs these too, and
            // reaching the wasm-bindgen CLI first trades our message for its
            // `failed to find __tls_align`.
            let missing = wasm_lite_codegen::missing_thread_exports(&module)?;
            if !missing.is_empty() {
                return Err(wasm_lite_codegen::missing_thread_exports_message(&missing));
            }
            if wasm_lite_codegen::spawns_without_shared_memory(&module)? {
                return Err(wasm_lite_codegen::spawns_without_shared_memory_message(
                    &module,
                ));
            }
            if wasm_lite_codegen::uses_wasm_bindgen(&module) {
                // The module also contains wasm-bindgen code: codegen finalizes
                // it with the wasm-bindgen CLI and assembles a merged loader.
                let bundle = wasm_lite_codegen::build_interop(program)?;
                routes.push(Route {
                    path: PROGRAM_JS,
                    content_type: "text/javascript; charset=utf-8",
                    body: bundle.loader_js.into_bytes(),
                });
                routes.push(Route {
                    path: WL_GLUE_JS,
                    content_type: "text/javascript; charset=utf-8",
                    body: bundle.wl_glue_js.into_bytes(),
                });
                routes.push(Route {
                    path: WB_GLUE_JS,
                    content_type: "text/javascript; charset=utf-8",
                    body: bundle.wb_glue_js.into_bytes(),
                });
                routes.push(Route {
                    path: PROGRAM_WASM,
                    content_type: "application/wasm",
                    body: bundle.wasm,
                });
                PROGRAM_JS
            } else {
                let descriptors = wasm_lite_codegen::descriptors_from_wasm(&module)?;
                let exports = wasm_lite_codegen::exports_from_wasm(&module)?;
                let memory = wasm_lite_codegen::imported_memory(&module)?;
                // Report a misconfigured link line here, where the message is
                // still attached to the failing target. Left to run, this dies
                // in the worker as a bare `TypeError` on `undefined`.
                let glue =
                    wasm_lite_codegen::generate_glue(&descriptors, &exports, memory.as_ref());
                // program.js is the glue ONLY (no auto-run), so a spawned worker
                // can import it. A separate bootstrap module runs `main`.
                routes.push(Route {
                    path: PROGRAM_JS,
                    content_type: "text/javascript; charset=utf-8",
                    body: glue.into_bytes(),
                });
                routes.push(Route {
                    path: PROGRAM_WASM,
                    content_type: "application/wasm",
                    body: module,
                });
                let bootstrap = "import { instantiate } from \"./program.js\";\n\
                     const instance = await instantiate(\"./program.wasm\");\n\
                     instance.exports.main();\n";
                routes.push(Route {
                    path: BOOTSTRAP_JS,
                    content_type: "text/javascript; charset=utf-8",
                    body: bootstrap.into(),
                });
                // Shared-memory builds spawn threads onto workers: serve the
                // worker bootstrap (it imports the glue at "./program.js").
                if memory.is_some() {
                    routes.push(Route {
                        path: WL_WORKER_JS,
                        content_type: "text/javascript; charset=utf-8",
                        body: wasm_lite_codegen::generate_worker("./program.js").into_bytes(),
                    });
                }
                BOOTSTRAP_JS
            }
        }
        other => {
            return Err(format!(
                "unsupported program type {other:?}; expected .js or .wasm"
            ));
        }
    };

    routes.insert(
        0,
        Route {
            path: "/",
            content_type: "text/html; charset=utf-8",
            body: shell::index_html(program, entry).into_bytes(),
        },
    );

    Ok(routes)
}

/// Read a file, mapping IO errors to a descriptive message.
fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))
}

/// Bind to localhost, preferring port 8080 but falling back to any free port.
fn bind() -> std::io::Result<TcpListener> {
    let preferred = SocketAddr::from((Ipv4Addr::LOCALHOST, 8080));
    match TcpListener::bind(preferred) {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
    }
}

/// Accept connections forever, serving the route table.
///
/// Each connection is handled on its own thread: browsers open speculative
/// connections they never send a request on (and `Connection: close` forces a
/// new connection per subresource), so serving sequentially can wedge the whole
/// server behind an idle socket until the browser tears it down — surfacing as
/// a spurious test-runner timeout. A read timeout bounds how long an idle
/// connection can hold its thread.
fn serve(listener: TcpListener, routes: &[Route]) -> ! {
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    scope.spawn(move || {
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                        if let Err(err) = handle(stream, routes) {
                            // Idle speculative connections time out here; not worth a warning.
                            if !matches!(
                                err.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) {
                                eprintln!("warning: request failed: {err}");
                            }
                        }
                    });
                }
                Err(err) => eprintln!("warning: connection failed: {err}"),
            }
        }
    });
    unreachable!("incoming() yields forever")
}

/// Handle a single HTTP request against the route table.
fn handle(mut stream: TcpStream, routes: &[Route]) -> std::io::Result<()> {
    let request = match read_request(&mut stream)? {
        Some(request) => request,
        None => return Ok(()),
    };

    // The WebSocket echo endpoint takes over the connection entirely: no
    // response body, no COOP/COEP headers, and the socket stays open.
    if request.path == ws_echo::PATH
        && let Some(key) = &request.ws_key
    {
        return ws_echo::serve(stream, key);
    }

    // Console forwarding. Answered before the route table and before
    // `WASM_LITE_SERVE_DIR`, so nothing on disk can shadow it, and with a
    // 200 either way: a `GET` of this path is not an error worth reporting to
    // a page that is only trying to talk to us.
    if request.path == LOG_PATH {
        if request.post {
            shell::print_log_batch(&request.body);
        }
        return respond(&mut stream, 200, "text/plain; charset=utf-8", b"", &request);
    }

    // Generated routes first — nothing on disk may shadow `program.wasm` or
    // the glue. Both are borrowed: a route body is the whole wasm module, and
    // copying it per request would be a real cost on every page load.
    let route = routes.iter().find(|r| r.path == request.path);
    let file = match route {
        Some(_) => None,
        None => static_file(&request.path),
    };
    let found = route
        .map(|r| (r.content_type, &r.body[..]))
        .or_else(|| file.as_ref().map(|(ct, body)| (*ct, &body[..])));
    let Some((content_type, body)) = found else {
        return respond(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"not found",
            &request,
        );
    };
    let Some((start, end)) = request.range else {
        return respond(&mut stream, 200, content_type, body, &request);
    };

    // Unsatisfiable: a range starting at or past the end, and a backwards one
    // (`bytes=10-5`). `async_file` reads a 416 as end-of-file rather than as an
    // error, which is what makes a sequential read terminate.
    //
    // The backwards case is not hypothetical politeness — without it the slice
    // below is `&body[10..=5]`, which panics and kills the connection thread.
    let len = body.len() as u64;
    let unsatisfiable = start >= len || end.is_some_and(|e| e < start);
    if unsatisfiable {
        return respond_range(&mut stream, 416, content_type, &[], None, len, &request);
    }
    // `end` is inclusive, and a client may ask for more than there is — a read
    // of the last kilobyte of a shorter file is normal, not an error.
    let last = end.unwrap_or(len - 1).min(len - 1);
    let slice = &body[start as usize..=last as usize];
    respond_range(
        &mut stream,
        206,
        content_type,
        slice,
        Some((start, last)),
        len,
        &request,
    )
}

/// Serve a file from `WASM_LITE_SERVE_DIR`, if one is set and the path is in it.
///
/// A real program wants more than its own module: textures, shaders, fonts, a
/// data file it fetches at startup. Without this the runner answers 404 for all
/// of them, and the program fails in a way that looks like a bug in its asset
/// code rather than a missing server.
///
/// Deliberately narrow: the generated routes always win, so nothing on disk can
/// shadow `program.wasm` or the glue.
fn static_file(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let root = PathBuf::from(std::env::var_os("WASM_LITE_SERVE_DIR")?);
    // Reject anything that could climb out of the root. `..` is the obvious
    // case; an absolute component or a Windows prefix would also escape, so
    // accept only plain names. This server is local, but it is pointed at a
    // directory the user names and reached by a browser, and "local" is not the
    // same as "only reachable by things I trust".
    let mut full = root.clone();
    for part in path.trim_start_matches('/').split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if !Path::new(part)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        full.push(part);
    }
    let full = full.canonicalize().ok()?;
    // Canonicalize both sides: a symlink inside the root can still point out of
    // it, and the component check above cannot see through one.
    if !full.starts_with(root.canonicalize().ok()?) {
        return None;
    }
    let body = std::fs::read(&full).ok()?;
    Some((content_type_for(&full), body))
}

/// Guess a Content-Type from the extension.
///
/// Only what a wasm program is likely to fetch. Anything else is served as
/// `application/octet-stream`, which is correct for `fetch`/`arrayBuffer` and
/// wrong only for a browser asked to display it directly.
fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "txt" | "" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "wgsl" | "glsl" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What a request asked for: its target path, its method, and any byte range.
struct Request {
    path: String,
    /// True for `HEAD`, whose response carries headers but no body.
    head: bool,
    /// True for `POST`, the only method that carries a body here.
    post: bool,
    /// The request body, empty unless one was sent. Clipped to
    /// [`MAX_REQUEST_BODY`].
    body: Vec<u8>,
    /// The first byte-range of a `Range: bytes=…` header, as `(start, end)`
    /// with `end` inclusive and `None` meaning "to the end of the resource".
    range: Option<(u64, Option<u64>)>,
    /// The `Sec-WebSocket-Key` of an upgrade request, if this is one.
    ws_key: Option<String>,
}

/// Read the request line and headers.
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    // "GET /path?query HTTP/1.1" — match on the path, ignoring any query string.
    let Some(path) = parts.next().map(|target| {
        target
            .split(['?', '#'])
            .next()
            .unwrap_or(target)
            .to_string()
    }) else {
        return Ok(None);
    };

    // Read headers up to the blank line, both to satisfy the client and to
    // pick up `Range`, the WebSocket upgrade key, and the body length.
    let mut range = None;
    let mut ws_key = None;
    let mut content_length = 0u64;
    let mut header = String::new();
    loop {
        header.clear();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("range") {
            range = parse_range(value);
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            ws_key = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
    }

    // The body has to be read here, while the buffered reader is still alive:
    // it has already pulled bytes past the headers off the socket, and dropping
    // it would take the start of the body with it. `take` rather than
    // `read_exact` so a client that promised more than it sent yields what it
    // did send instead of an error.
    let mut body = Vec::new();
    if content_length > 0 {
        reader
            .take(content_length.min(MAX_REQUEST_BODY))
            .read_to_end(&mut body)?;
    }

    Ok(Some(Request {
        path,
        head: method.eq_ignore_ascii_case("HEAD"),
        post: method.eq_ignore_ascii_case("POST"),
        body,
        range,
        ws_key,
    }))
}

/// Parse the first range of a `Range: bytes=start-end` header.
///
/// Only the single-range `bytes` form, which is what a `fetch` with a `Range`
/// header sends. Multi-range requests (`bytes=0-9,20-29`) would need a
/// multipart response body; a server that answered one with a single range
/// would be lying about what it sent, so they are refused (`None`) and served
/// whole instead.
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    // A suffix range (`bytes=-500`, the last 500 bytes) needs the resource
    // length to resolve, which this parser does not have. Not needed by
    // anything here, so it is refused rather than mis-resolved.
    let start: u64 = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

/// Write a complete HTTP/1.1 response and close the connection.
fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    request: &Request,
) -> std::io::Result<()> {
    let len = body.len() as u64;
    respond_range(stream, status, content_type, body, None, len, request)
}

/// As [`respond`], with the `Content-Range`/`Accept-Ranges` a partial response
/// needs.
///
/// `content_range` is `(first, last)` of what is being sent, inclusive;
/// `complete_len` is the length of the whole resource.
fn respond_range(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    content_range: Option<(u64, u64)>,
    complete_len: u64,
    request: &Request,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Unknown",
    };
    let range_header = match (status, content_range) {
        (206, Some((first, last))) => {
            format!("Content-Range: bytes {first}-{last}/{complete_len}\r\n")
        }
        // A 416 says what the resource's length actually is, so a client can
        // retry with a range that exists.
        (416, _) => format!("Content-Range: bytes */{complete_len}\r\n"),
        _ => String::new(),
    };
    // Cross-origin isolation headers: browsers only expose `SharedArrayBuffer`
    // (and thus shared linear memory for `+atomics` builds) to isolated pages.
    // Harmless for the non-shared examples.
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Accept-Ranges: bytes\r\n\
         {range_header}\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len()
    );
    stream.write_all(header.as_bytes())?;
    // A HEAD response carries the headers a GET would and no body — including
    // the `Content-Length` the GET would have, which is the whole reason to
    // send one. Writing the body anyway is what the runner used to do, and it
    // desyncs a client that trusts the method.
    if !request.head {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// Open the given URL in the system default browser.
/// Ask the desktop to open `url`, reporting why if it could not.
///
/// A non-zero exit counts as failure, not just a failure to spawn: on a
/// headless box `xdg-open` runs fine and exits 3 having found no browser to
/// hand the URL to, which used to read as success.
fn open_browser(url: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };

    match result {
        Err(err) => Err(err.to_string()),
        Ok(status) if !status.success() => Err(format!("the opener exited with {status}")),
        Ok(_) => Ok(()),
    }
}
