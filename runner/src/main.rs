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

/// Path under which the HTML shell loads the program (an ES module).
const PROGRAM_JS: &str = "/program.js";
/// Path under which a wasm module is served.
const PROGRAM_WASM: &str = "/program.wasm";

/// A single static resource served by the runner.
struct Route {
    path: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn main() {
    let program = match parse_args() {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("usage: runner <program.js|program.wasm>");
            std::process::exit(2);
        }
    };

    let routes = match build_routes(&program) {
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
    open_browser(&url);
    println!("opening browser... (ctrl-c to stop)");

    serve(listener, &routes);
}

/// Parse command-line arguments into the program path.
fn parse_args() -> Result<PathBuf, String> {
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or_else(|| "error: missing program path".to_string())?;
    if args.next().is_some() {
        return Err("error: too many arguments".to_string());
    }
    Ok(PathBuf::from(program))
}

/// Build the route table for the given program, dispatching on its extension.
fn build_routes(program: &Path) -> Result<Vec<Route>, String> {
    let ext = program.extension().and_then(|e| e.to_str()).unwrap_or("");
    let html = index_html(program).into_bytes();

    let mut routes = vec![Route {
        path: "/",
        content_type: "text/html; charset=utf-8",
        body: html,
    }];

    match ext {
        "js" => {
            let source = read(program)?;
            routes.push(Route {
                path: PROGRAM_JS,
                content_type: "text/javascript; charset=utf-8",
                body: source,
            });
        }
        "wasm" => {
            let module = read(program)?;
            let descriptors = wasm_lite_codegen::descriptors_from_wasm(&module)?;
            let glue = wasm_lite_codegen::generate_glue(&descriptors);
            // The codegen glue exports `instantiate`; the runner appends a
            // bootstrap that runs the module's `main`.
            let program_js = format!(
                "{glue}\nconst instance = await instantiate({PROGRAM_WASM:?});\ninstance.exports.main();\n"
            );
            routes.push(Route {
                path: PROGRAM_JS,
                content_type: "text/javascript; charset=utf-8",
                body: program_js.into_bytes(),
            });
            routes.push(Route {
                path: PROGRAM_WASM,
                content_type: "application/wasm",
                body: module,
            });
        }
        other => {
            return Err(format!(
                "unsupported program type {other:?}; expected .js or .wasm"
            ));
        }
    }

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

/// The HTML shell that loads the program as an ES module.
fn index_html(program: &Path) -> String {
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
         <script type=\"module\" src=\"{PROGRAM_JS}\"></script>\n\
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

    // "GET /path HTTP/1.1"
    let path = request_line.split_whitespace().nth(1).map(str::to_string);

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
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
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
