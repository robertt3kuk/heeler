//! A scriptable HTTP/1.1 server on a loopback port for tests (feature `test-support`): replies
//! are queued per `(method, path)`, streamed in steps (with pauses, holds and resets), and every
//! request is recorded. Written by hand so the adapters are tested against the byte shapes the
//! providers document, not against another HTTP stack's idea of them.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// One step of a streamed reply.
#[derive(Debug, Clone)]
pub enum Step {
    /// Write these bytes (one HTTP chunk when the reply is chunked).
    Send(Vec<u8>),
    Sleep(Duration),
    /// Keep the connection open until the client closes it.
    Hold,
    /// Close the connection without finishing the body.
    Reset,
}

/// What the server answers a request with.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub steps: Vec<Step>,
    chunked: bool,
}

impl Reply {
    /// A complete body with a `content-length`.
    pub fn fixed(status: u16, content_type: &str, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            steps: vec![Step::Send(body.as_bytes().to_vec())],
            chunked: false,
        }
    }

    pub fn json(status: u16, body: &str) -> Self {
        Self::fixed(status, "application/json", body)
    }

    /// A chunked `text/event-stream`; each item is written as one chunk.
    pub fn sse<S: AsRef<str>>(chunks: impl IntoIterator<Item = S>) -> Self {
        Self::stream("text/event-stream", chunks)
    }

    /// A chunked `application/x-ndjson`.
    pub fn ndjson<S: AsRef<str>>(chunks: impl IntoIterator<Item = S>) -> Self {
        Self::stream("application/x-ndjson", chunks)
    }

    fn stream<S: AsRef<str>>(content_type: &str, chunks: impl IntoIterator<Item = S>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".into(), content_type.into())],
            steps: chunks
                .into_iter()
                .map(|c| Step::Send(c.as_ref().as_bytes().to_vec()))
                .collect(),
            chunked: true,
        }
    }

    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn then(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// After the body written so far, keep the connection open (the reply never completes).
    #[must_use]
    pub fn then_hold(self) -> Self {
        self.then(Step::Hold)
    }

    /// After the body written so far, drop the connection mid-body.
    #[must_use]
    pub fn then_reset(self) -> Self {
        self.then(Step::Reset)
    }
}

/// A request the server received.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// Path and query as sent.
    pub target: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }

    /// The body as JSON (`Null` when it is not).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The whole request as text: what a secret scan looks through.
    pub fn text(&self) -> String {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(n, v)| format!("{n}: {v}"))
            .collect();
        format!(
            "{} {}\n{}\n\n{}",
            self.method,
            self.target,
            headers.join("\n"),
            String::from_utf8_lossy(&self.body)
        )
    }
}

struct Route {
    method: String,
    path: String,
    replies: VecDeque<Reply>,
}

#[derive(Default)]
struct Shared {
    routes: Mutex<Vec<Route>>,
    requests: Mutex<Vec<RecordedRequest>>,
    connections: AtomicUsize,
    disconnects: AtomicUsize,
    changed: Notify,
}

/// The server. Dropping it stops it.
#[derive(Debug)]
pub struct MockServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

impl MockServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("binding the mock server: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("the mock server's address: {e}"));
        let shared = Arc::new(Shared::default());
        let task = tokio::spawn(accept(listener, shared.clone()));
        Self { addr, shared, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Queues a reply for `method path`. Replies are served in order; the last one repeats.
    pub fn route(&self, method: &str, path: &str, reply: Reply) {
        let mut routes = self.shared.routes.lock();
        match routes
            .iter_mut()
            .find(|r| r.method == method && r.path == path)
        {
            Some(route) => route.replies.push_back(reply),
            None => routes.push(Route {
                method: method.to_owned(),
                path: path.to_owned(),
                replies: VecDeque::from([reply]),
            }),
        }
    }

    /// Replaces everything queued for `method path` with `reply`.
    pub fn set_route(&self, method: &str, path: &str, reply: Reply) {
        self.shared
            .routes
            .lock()
            .retain(|r| !(r.method == method && r.path == path));
        self.route(method, path, reply);
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.requests.lock().clone()
    }

    /// Requests received for `path`.
    pub fn requests_to(&self, path: &str) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path() == path)
            .collect()
    }

    /// TCP connections accepted so far.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Connections the client closed (or that broke) while the server was still writing or
    /// holding them.
    pub fn disconnects(&self) -> usize {
        self.shared.disconnects.load(Ordering::SeqCst)
    }

    /// Waits until `count` requests were received for `path`.
    pub async fn wait_for_requests(&self, path: &str, count: usize) {
        loop {
            let notified = self.shared.changed.notified();
            if self.requests_to(path).len() >= count {
                return;
            }
            notified.await;
        }
    }

    /// Waits until `count` client disconnects were seen.
    pub async fn wait_for_disconnects(&self, count: usize) {
        loop {
            let notified = self.shared.changed.notified();
            if self.disconnects() >= count {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        shared.connections.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(serve(stream, shared.clone()));
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(RecordedRequest {
        method,
        target,
        headers,
        body,
    })
}

fn next_reply(shared: &Shared, request: &RecordedRequest) -> Reply {
    let mut routes = shared.routes.lock();
    let route = routes
        .iter_mut()
        .find(|r| r.method == request.method && r.path == request.path());
    match route {
        Some(route) if route.replies.len() > 1 => route.replies.pop_front(),
        Some(route) => route.replies.front().cloned(),
        None => None,
    }
    .unwrap_or_else(|| {
        Reply::json(
            404,
            r#"{"error":{"message":"no mock route for this request"}}"#,
        )
    })
}

async fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let reply = next_reply(&shared, &request);
    shared.requests.lock().push(request);
    shared.changed.notify_waiters();

    let disconnected = || {
        shared.disconnects.fetch_add(1, Ordering::SeqCst);
        shared.changed.notify_waiters();
    };
    let reason = match reply.status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nconnection: close\r\n",
        reply.status
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let body_len: usize = reply
        .steps
        .iter()
        .map(|s| if let Step::Send(b) = s { b.len() } else { 0 })
        .sum();
    if reply.chunked {
        head.push_str("transfer-encoding: chunked\r\n");
    } else {
        head.push_str(&format!("content-length: {body_len}\r\n"));
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).await.is_err() {
        disconnected();
        return;
    }
    for step in &reply.steps {
        match step {
            Step::Send(bytes) => {
                let written = if reply.chunked {
                    let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
                    framed.extend_from_slice(bytes);
                    framed.extend_from_slice(b"\r\n");
                    stream.write_all(&framed).await
                } else {
                    stream.write_all(bytes).await
                };
                if written.is_err() || stream.flush().await.is_err() {
                    disconnected();
                    return;
                }
            }
            Step::Sleep(pause) => tokio::time::sleep(*pause).await,
            Step::Hold => {
                let mut sink = [0u8; 64];
                // The client sends nothing more; any read result ends the hold (EOF or error
                // is the client going away).
                while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {}
                disconnected();
                return;
            }
            Step::Reset => return,
        }
    }
    if reply.chunked && stream.write_all(b"0\r\n\r\n").await.is_err() {
        disconnected();
        return;
    }
    let _ = stream.shutdown().await;
}
