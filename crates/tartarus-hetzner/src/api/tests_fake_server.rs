//! Minimal HTTP/1.1 server used by the API-client unit tests.
//!
//! Spins up a `TcpListener` on `127.0.0.1:0`, returns the base URL,
//! and answers one request per script entry with a canned response.
//! The server runs on its own thread and shuts down when its
//! [`Server`] drop-guard does.
//!
//! Out of scope: chunked transfer, HTTP/2, keep-alive across multiple
//! exchanges. Every Tartarus call is a one-shot request-response, so
//! the server reads the full request body (Content-Length-aware) and
//! then closes the socket.

#![cfg(test)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

// -----------------------------------------------------------------------------
// CannedResponse
// -----------------------------------------------------------------------------

/// One scripted HTTP response.
#[derive(Clone, Debug)]
pub struct CannedResponse {
    /// HTTP status line code (e.g. 200, 201, 204).
    pub status: u16,

    /// Response body. `204 No Content` callers should pass `""`.
    pub body: String,
}

impl CannedResponse {
    /// Shortcut for the canonical Hetzner success: `201 Created` +
    /// JSON body.
    pub fn created(body: impl Into<String>) -> Self {
        Self {
            status: 201,
            body: body.into(),
        }
    }

    /// Shortcut for `200 OK` + JSON body.
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }

    /// Shortcut for `204 No Content` (empty body).
    pub fn no_content() -> Self {
        Self {
            status: 204,
            body: String::new(),
        }
    }

    /// Shortcut for a Hetzner-shaped error response.
    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self {
            status,
            body: format!(r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#),
        }
    }
}

// -----------------------------------------------------------------------------
// Server
// -----------------------------------------------------------------------------

/// One scripted HTTP exchange the server will answer with.
type Script = Vec<CannedResponse>;

/// Drop-guard around the listener thread.
pub struct Server {
    /// `http://127.0.0.1:<port>` the test points its `Client` at.
    pub base_url: String,

    /// Captured request lines (method + path) so tests can assert
    /// that the right endpoint was hit.
    pub seen_paths: Arc<Mutex<Vec<String>>>,

    /// Captured request bodies so tests can assert payload shape.
    pub seen_bodies: Arc<Mutex<Vec<String>>>,

    /// Thread handle the drop impl joins to flush any panics.
    handle: Option<JoinHandle<()>>,
}

impl Server {
    /// Spin up a server that will answer scripted responses one at a
    /// time. Subsequent requests after the script is exhausted get a
    /// 500 with `"script exhausted"`.
    pub fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind to a free loopback port");
        let port = listener.local_addr().expect("listener has a local address").port();
        let base_url = format!("http://127.0.0.1:{port}");

        let seen_paths = Arc::new(Mutex::new(Vec::new()));
        let seen_bodies = Arc::new(Mutex::new(Vec::new()));
        let seen_paths_thread = seen_paths.clone();
        let seen_bodies_thread = seen_bodies.clone();

        let handle = thread::spawn(move || {
            let mut script_iter = script.into_iter();
            for stream in listener.incoming().flatten() {
                let (path, body, keep_running) = handle_one(stream, &mut script_iter);
                seen_paths_thread.lock().expect("path lock").push(path);
                seen_bodies_thread.lock().expect("body lock").push(body);
                if !keep_running {
                    break;
                }
            }
        });

        Self {
            base_url,
            seen_paths,
            seen_bodies,
            handle: Some(handle),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Make one final connection so the accept loop notices the
        // listener has been closed once the thread joins. The
        // listener is dropped when the spawned thread returns.
        let _ = std::net::TcpStream::connect(self.base_url.strip_prefix("http://").expect("base_url is http://..."));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// -----------------------------------------------------------------------------
// Wire Handling
// -----------------------------------------------------------------------------

/// Read one HTTP request from `stream`, write the next scripted
/// response. Returns `(request_line + path, body, keep_listening)`.
fn handle_one(
    stream: std::net::TcpStream,
    script: &mut impl Iterator<Item = CannedResponse>,
) -> (String, String, bool) {
    let peer = stream.peer_addr().ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;

    // Request line.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return (String::new(), String::new(), false);
    }
    let request_line = request_line.trim_end_matches(['\r', '\n']).to_owned();

    // Headers.
    let mut content_length: usize = 0;
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).is_err() {
            break;
        }
        if header_line == "\r\n" || header_line == "\n" || header_line.is_empty() {
            break;
        }
        let header_line = header_line.trim_end_matches(['\r', '\n']).to_lowercase();
        if let Some(rest) = header_line.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }

    // Body.
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    let body_str = String::from_utf8_lossy(&body).into_owned();

    // Response.
    let response = script.next().unwrap_or_else(|| CannedResponse {
        status: 500,
        body: "script exhausted".to_owned(),
    });

    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Status",
    };
    let _ = writeln!(
        writer,
        "HTTP/1.1 {status} {text}\r\nContent-Length: {len}\r\nContent-Type: application/json\r\nConnection: close\r\n\r",
        status = response.status,
        text = status_text,
        len = response.body.len(),
    );
    let _ = writer.write_all(response.body.as_bytes());
    let _ = writer.flush();
    let _ = peer; // suppress unused when not logging

    (request_line, body_str, true)
}
