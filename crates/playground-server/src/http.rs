//! The TCP layer of the playground server (issue 038): request-line
//! parsing with bounded sizes, response serialization, and the accept
//! loop with graceful shutdown. All policy decisions live in the pure
//! [`route`](crate::route) function; this module only moves bytes.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{Registry, route};

/// Why a request could not be turned into a [`crate::Request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReqError {
    /// Malformed request line / headers / body length.
    BadRequest,
    /// `Content-Length` beyond the configured bound (HTTP 413).
    TooLarge,
}

/// Parse one HTTP/1.1 request from `buf` (request line, headers, body
/// of exactly `Content-Length` bytes).
///
/// # Errors
/// [`ReqError::BadRequest`] on malformed input (including a body shorter
/// than `Content-Length`), [`ReqError::TooLarge`] when the declared
/// length exceeds `max_body`.
pub fn parse_request(buf: &[u8], max_body: usize) -> Result<crate::Request, ReqError> {
    let header_end = find_header_end(buf).ok_or(ReqError::BadRequest)?;
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| ReqError::BadRequest)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(ReqError::BadRequest)?;
    let mut parts = request_line.split(' ');
    let method = match parts.next() {
        Some("GET") => crate::Method::Get,
        Some("POST") => crate::Method::Post,
        _ => return Err(ReqError::BadRequest),
    };
    let path = parts.next().ok_or(ReqError::BadRequest)?.to_owned();
    let version = parts.next().ok_or(ReqError::BadRequest)?;
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ReqError::BadRequest);
    }

    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ReqError::BadRequest);
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().map_err(|_| ReqError::BadRequest)?;
        }
    }
    if content_length > max_body {
        return Err(ReqError::TooLarge);
    }
    let body_start = header_end + 4;
    let body_end = body_start
        .checked_add(content_length)
        .ok_or(ReqError::BadRequest)?;
    if buf.len() < body_end {
        return Err(ReqError::BadRequest);
    }
    Ok(crate::Request {
        method,
        path,
        body: buf[body_start..body_end].to_vec(),
    })
}

/// Find the `\r\n\r\n` boundary; returns the offset of the `\r` that
/// starts it.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Serialize a response onto the wire: status line, fixed headers,
/// `Content-Length`, `Connection: close`, body.
#[must_use]
pub fn serialize_response(resp: &crate::Response) -> Vec<u8> {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        reason,
        resp.body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(resp.body.as_bytes());
    out
}

/// The server: registry + policy + shutdown flag, serving one listener.
pub struct Server {
    /// The pack registry.
    pub registry: Registry,
    /// Maximum accepted `Content-Length` (413 beyond).
    pub max_body: usize,
    /// Set to `true` to end the accept loop gracefully.
    pub shutdown: Arc<AtomicBool>,
    /// How long a shutdown-flag poll sleeps between accepts.
    pub poll: Duration,
}

impl Server {
    /// Serve until the shutdown flag is set. Non-blocking accept with a
    /// poll interval keeps shutdown latency at one poll tick.
    ///
    /// # Errors
    /// Propagates accept errors other than `WouldBlock`.
    pub fn serve(&self, listener: &TcpListener) -> std::io::Result<()> {
        listener.set_nonblocking(true)?;
        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            match listener.accept() {
                Ok((stream, _addr)) => self.handle(stream),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(self.poll);
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Read one request, route it, write the response, close.
    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Some(buf) = read_request(&mut stream, self.max_body) else {
            return; // client vanished or sent garbage — drop the connection
        };
        let response = match parse_request(&buf, self.max_body) {
            Ok(req) => route(&self.registry, self.max_body, &req),
            Err(ReqError::TooLarge) => crate::Response {
                status: 413,
                body: "{\"error\":\"request body too large\"}".to_owned(),
            },
            Err(ReqError::BadRequest) => crate::Response {
                status: 400,
                body: "{\"error\":\"malformed request\"}".to_owned(),
            },
        };
        let _ = stream.write_all(&serialize_response(&response));
        let _ = stream.flush();
        // `Connection: close`: drop closes the socket, the client sees EOF.
    }
}

/// Read from the stream until a complete request is buffered. Bails out
/// once the header block or the declared body would exceed the bound —
/// the caller answers 413/400 without buffering forever.
fn read_request(stream: &mut TcpStream, max_body: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_cap = max_body + 16 * 1024;
    loop {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            // Headers complete: keep reading until Content-Length is
            // satisfied (parse_request validates the full length).
            let ok = parse_request(&buf, max_body);
            match ok {
                // Complete, or oversized — either way hand it up; the
                // router answers 200 or 413.
                Ok(_) | Err(ReqError::TooLarge) => return Some(buf),
                // May simply be a short body — read more.
                Err(ReqError::BadRequest) => {}
            }
        }
        if buf.len() > header_cap {
            return Some(buf); // oversized: let parse_request report 413
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Some(buf), // EOF: hand whatever we have up
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Some(buf);
            }
            Err(_) => return None,
        }
    }
}
