//! wasm_lite runner.
//!
//! Serves a single program over a local HTTP server and opens it in the
//! system default browser. For this first milestone the program is plain JS;
//! the runner is intentionally dependency-free and built on `std` only.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path under which the served HTML references the program.
const PROGRAM_PATH: &str = "/program.js";

fn main() {
    let program = match parse_args() {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("usage: runner <program.js>");
            std::process::exit(2);
        }
    };

    let source = match std::fs::read(&program) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("error: failed to read {}: {err}", program.display());
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

    let html = index_html(&program);
    serve(listener, html.as_bytes(), &source);
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
    let title = program.file_name().and_then(|n| n.to_str()).unwrap_or("program");
    format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <title>wasm_lite runner — {title}</title>\n\
         </head>\n\
         <body>\n\
         <pre id=\"output\"></pre>\n\
         <script type=\"module\" src=\"{PROGRAM_PATH}\"></script>\n\
         </body>\n\
         </html>\n"
    )
}

/// Accept connections forever, serving the HTML shell and the program.
fn serve(listener: TcpListener, html: &[u8], program: &[u8]) -> ! {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle(stream, html, program) {
                    eprintln!("warning: request failed: {err}");
                }
            }
            Err(err) => eprintln!("warning: connection failed: {err}"),
        }
    }
    unreachable!("incoming() yields forever")
}

/// Handle a single HTTP request.
fn handle(mut stream: TcpStream, html: &[u8], program: &[u8]) -> std::io::Result<()> {
    let path = match read_request_target(&mut stream)? {
        Some(path) => path,
        None => return Ok(()),
    };

    match path.as_str() {
        "/" => respond(&mut stream, 200, "text/html; charset=utf-8", html),
        PROGRAM_PATH => respond(&mut stream, 200, "text/javascript; charset=utf-8", program),
        _ => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found"),
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
fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> std::io::Result<()> {
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
