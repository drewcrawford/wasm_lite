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

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
mod test_runner;
mod webdriver;

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

/// A single static resource served by the runner.
struct Route {
    path: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn main() {
    // Stop a persistent (reused) browser started via `WASM_LITE_REUSE_BROWSER`.
    if std::env::args().any(|a| a == "--stop-browser") {
        webdriver::Browser::stop_persistent();
        println!("stopped the persistent browser");
        return;
    }

    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("usage: runner [--serve] <program.js|program.wasm>");
            std::process::exit(2);
        }
    };

    // Run headless and exit with a status code for `cargo test` and doctests;
    // serve + open a browser for `cargo run` and direct/interactive use. The two
    // are told apart by how Cargo invokes us (see `is_test_run`).
    if is_test_run(&args) {
        std::process::exit(test_runner::run(&args.program));
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
    if args.test {
        return true;
    }

    let path = args.program.to_string_lossy();
    if path.contains("/deps/") || path.contains("rustdoctest") {
        return true;
    }

    std::fs::read(&args.program)
        .map(|module| !wasm_lite_codegen::test_names(&module).is_empty())
        .unwrap_or(false)
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
        open_browser(&url);
        println!("opening browser... (ctrl-c to stop)");
    }

    serve(listener, &routes);
}

/// Parsed command-line arguments.
struct Args {
    program: PathBuf,
    serve: bool,
    test: bool,
}

/// Parse command-line arguments.
///
/// The first non-flag argument is the program (`.js` or `.wasm`). `--serve`
/// forces the interactive server; `--test` forces headless test mode. Other
/// flags (e.g. test-harness args Cargo appends) are ignored, so the runner
/// works directly as a Cargo runner (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER`),
/// invoked as `runner <artifact.wasm> [harness args…]`.
fn parse_args() -> Result<Args, String> {
    let mut program = None;
    let mut serve = false;
    let mut test = false;
    for arg in std::env::args_os().skip(1) {
        let text = arg.to_string_lossy();
        if text == "--serve" {
            serve = true;
        } else if text == "--test" {
            test = true;
        } else if text.starts_with('-') {
            // Ignore other flags (e.g. test-harness arguments).
        } else if program.is_none() {
            program = Some(PathBuf::from(arg));
        }
    }
    Ok(Args {
        program: program.ok_or_else(|| "error: missing program path".to_string())?,
        serve,
        test,
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
                let glue = wasm_lite_codegen::generate_glue(&descriptors, &exports, memory.as_ref());
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
            body: index_html(program, entry).into_bytes(),
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

/// The HTML shell that loads the program as an ES module from `entry`.
fn index_html(program: &Path, entry: &str) -> String {
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
         <pre id=\"output\"></pre>\n\
         <script>{CONSOLE_MIRROR}</script>\n\
         <script type=\"module\" src=\"{entry}\"></script>\n\
         </body>\n\
         </html>\n"
    )
}

/// Classic (non-module) script that mirrors `console` output onto the page, so
/// the visible window shows whatever the program logs. Runs before the module.
const CONSOLE_MIRROR: &str = r#"
for (const level of ["log", "error", "warn", "info"]) {
    const original = console[level].bind(console);
    console[level] = (...args) => {
        original(...args);
        const out = document.getElementById("output");
        if (out) { out.textContent += args.join(" ") + "\n"; }
    };
}
"#;

/// Accept connections forever, serving the route table.
fn serve(listener: TcpListener, routes: &[Route]) -> ! {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle(stream, routes) {
                    eprintln!("warning: request failed: {err}");
                }
            }
            Err(err) => eprintln!("warning: connection failed: {err}"),
        }
    }
    unreachable!("incoming() yields forever")
}

/// Handle a single HTTP request against the route table.
fn handle(mut stream: TcpStream, routes: &[Route]) -> std::io::Result<()> {
    let path = match read_request_target(&mut stream)? {
        Some(path) => path,
        None => return Ok(()),
    };

    match routes.iter().find(|r| r.path == path) {
        Some(route) => respond(&mut stream, 200, route.content_type, &route.body),
        None => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

/// Read the request line and return its target path; drains remaining headers.
fn read_request_target(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }

    // "GET /path?query HTTP/1.1" — match on the path, ignoring any query string.
    let path = request_line
        .split_whitespace()
        .nth(1)
        .map(|target| target.split(['?', '#']).next().unwrap_or(target).to_string());

    // Drain headers up to the blank line so the client is satisfied.
    let mut header = String::new();
    loop {
        header.clear();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }

    Ok(path)
}

/// Write a complete HTTP/1.1 response and close the connection.
fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Unknown",
    };
    // Cross-origin isolation headers: browsers only expose `SharedArrayBuffer`
    // (and thus shared linear memory for `+atomics` builds) to isolated pages.
    // Harmless for the non-shared examples.
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Open the given URL in the system default browser.
fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };

    if let Err(err) = result {
        eprintln!("warning: could not open browser automatically: {err}");
        eprintln!("open this URL manually: {url}");
    }
}
