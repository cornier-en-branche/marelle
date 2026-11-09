// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Helpers for tests across the workspace. Not published.
//!
//! [`MockServer`] is a small HTTP/1.1 server built on `std` alone. It answers
//! with scripted replies and records what it received, so provider adapters
//! can be tested end to end without a real provider or a real API key.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One request the server received.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// The HTTP method.
    pub method: String,
    /// The request target, such as `/v1/chat/completions`.
    pub path: String,
    /// Header names (lowercased) and values, in the order received.
    pub headers: Vec<(String, String)>,
    /// The request body as text.
    pub body: String,
}

impl RecordedRequest {
    /// The value of a header, looked up without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// What the server does with a request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A complete response with a body.
    Body {
        /// The HTTP status code.
        status: u16,
        /// The `Content-Type` header.
        content_type: String,
        /// The response body.
        body: String,
    },
    /// A `text/event-stream` response sent in chunks, pausing after each.
    Stream {
        /// The chunks, sent and flushed one at a time.
        chunks: Vec<String>,
        /// How long to wait after each chunk.
        pause: Duration,
    },
    /// A stream that dies: the socket closes after the chunks, with no proper
    /// end of the chunked body.
    Abort {
        /// The chunks sent before the socket closes.
        chunks: Vec<String>,
    },
    /// Sends the response headers, then stays silent for this long.
    Stall(Duration),
    /// A redirect to another address.
    Redirect {
        /// The status code, such as 307.
        status: u16,
        /// The `Location` header.
        location: String,
    },
}

impl Reply {
    /// A JSON response.
    pub fn json(status: u16, body: &str) -> Reply {
        Reply::Body {
            status,
            content_type: "application/json".to_owned(),
            body: body.to_owned(),
        }
    }

    /// An event stream whose chunks arrive back to back.
    pub fn sse<S: AsRef<str>>(chunks: impl IntoIterator<Item = S>) -> Reply {
        Reply::Stream {
            chunks: chunks.into_iter().map(|c| c.as_ref().to_owned()).collect(),
            pause: Duration::ZERO,
        }
    }

    /// An event stream that dies after the chunks.
    pub fn sse_abort<S: AsRef<str>>(chunks: impl IntoIterator<Item = S>) -> Reply {
        Reply::Abort {
            chunks: chunks.into_iter().map(|c| c.as_ref().to_owned()).collect(),
        }
    }

    /// A redirect.
    pub fn redirect(status: u16, location: &str) -> Reply {
        Reply::Redirect {
            status,
            location: location.to_owned(),
        }
    }
}

/// A local HTTP server with scripted replies.
///
/// The nth request gets the nth reply, and the last reply repeats. The server
/// stops when the value is dropped.
pub struct MockServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
}

impl MockServer {
    /// Starts a server on a free local port.
    ///
    /// # Panics
    ///
    /// Panics when `replies` is empty or no local port can be bound.
    pub fn start(replies: Vec<Reply>) -> MockServer {
        assert!(
            !replies.is_empty(),
            "a mock server needs at least one reply"
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let addr = listener.local_addr().expect("local address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let replies = Arc::new(replies);
        let counter = Arc::new(AtomicUsize::new(0));

        let (log, flag) = (Arc::clone(&requests), Arc::clone(&stop));
        thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (log, replies, counter) =
                    (Arc::clone(&log), Arc::clone(&replies), Arc::clone(&counter));
                thread::spawn(move || {
                    // A client that hangs up early is not a test failure.
                    let _ = serve(stream, &log, &replies, &counter);
                });
            }
        });

        MockServer {
            addr,
            requests,
            stop,
        }
    }

    /// The address of the server, such as `http://127.0.0.1:41234`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("request log lock").clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so its thread can end.
        let _ = TcpStream::connect(self.addr);
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        307 => "Temporary Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn serve(
    stream: TcpStream,
    log: &Mutex<Vec<RecordedRequest>>,
    replies: &[Reply],
    counter: &AtomicUsize,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }

    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;

    log.lock().expect("request log lock").push(RecordedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });

    let nth = counter.fetch_add(1, Ordering::SeqCst);
    let reply = &replies[nth.min(replies.len() - 1)];
    let mut out = stream;
    match reply {
        Reply::Body {
            status,
            content_type,
            body,
        } => {
            write!(
                out,
                "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                reason(*status),
                body.len()
            )?;
            out.flush()
        }
        Reply::Stream { chunks, pause } => {
            write_stream_head(&mut out)?;
            for chunk in chunks {
                write_chunk(&mut out, chunk)?;
                thread::sleep(*pause);
            }
            out.write_all(b"0\r\n\r\n")?;
            out.flush()
        }
        Reply::Abort { chunks } => {
            write_stream_head(&mut out)?;
            for chunk in chunks {
                write_chunk(&mut out, chunk)?;
            }
            // Dropping the socket here ends the body without the closing chunk.
            Ok(())
        }
        Reply::Stall(stall) => {
            write_stream_head(&mut out)?;
            thread::sleep(*stall);
            Ok(())
        }
        Reply::Redirect { status, location } => {
            write!(
                out,
                "HTTP/1.1 {status} {}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                reason(*status)
            )?;
            out.flush()
        }
    }
}

fn write_stream_head(out: &mut TcpStream) -> io::Result<()> {
    out.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    )?;
    out.flush()
}

fn write_chunk(out: &mut TcpStream, chunk: &str) -> io::Result<()> {
    write!(out, "{:x}\r\n{chunk}\r\n", chunk.len())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(addr: &str, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect");
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: x\r\nX-Test: yes\r\nContent-Length: 5\r\n\r\nhello"
        )
        .expect("send");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("read");
        answer
    }

    #[test]
    fn records_requests_and_replies_in_order() {
        let server = MockServer::start(vec![Reply::json(200, "{\"a\":1}"), Reply::json(404, "no")]);
        let addr = server.base_url().replace("http://", "");

        let first = get(&addr, "/one");
        assert!(first.starts_with("HTTP/1.1 200 OK"));
        assert!(first.ends_with("{\"a\":1}"));
        let second = get(&addr, "/two");
        assert!(second.starts_with("HTTP/1.1 404 Not Found"));
        let third = get(&addr, "/three");
        assert!(third.starts_with("HTTP/1.1 404"), "the last reply repeats");

        let seen = server.requests();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].path, "/one");
        assert_eq!(seen[0].header("X-TEST"), Some("yes"));
        assert_eq!(seen[0].body, "hello");
    }

    #[test]
    fn streams_chunked_events() {
        let server = MockServer::start(vec![Reply::sse(["data: a\n\n", "data: b\n\n"])]);
        let answer = get(&server.base_url().replace("http://", ""), "/s");
        assert!(answer.contains("Transfer-Encoding: chunked"));
        assert!(answer.contains("data: a"));
        assert!(answer.ends_with("0\r\n\r\n"));
    }
}
