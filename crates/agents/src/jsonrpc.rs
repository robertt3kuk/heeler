//! Newline-delimited JSON-RPC over a child process's stdio (the shape `codex app-server` speaks;
//! no `jsonrpc` member). One reader task classifies every line: responses resolve the pending
//! request, everything else goes to the bounded [`Incoming`] queue.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{debug, warn};

use crate::adapter::ExitInfo;
use crate::error::AgentError;

/// Longest line accepted from the process (a `thread/resume` with full history can be big).
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Messages waiting for the consumer before the reader (and thereby the process) waits.
const INCOMING_QUEUE: usize = 256;
const OUTGOING_QUEUE: usize = 64;
/// stderr lines remembered for error messages.
const STDERR_LINES: usize = 20;
/// How long a request may wait for its response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a process gets to leave after its stdin closed before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// How to start the process: `<program> <args...>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// What [`Transport::spawn`] returns: the sending half, the stream of the process's own
/// messages, and the eventual exit status.
pub type Spawned = (
    Arc<Transport>,
    mpsc::Receiver<Incoming>,
    oneshot::Receiver<ExitInfo>,
);

/// A message from the process that is not a response.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// The process asks us something; answer with [`Transport::respond`] or
    /// [`Transport::respond_error`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The output ended: the process is gone or closed stdout.
    Closed,
}

/// Requests waiting for a response. Once the output has ended nothing can be added: a request
/// made after that would wait for an answer that cannot come.
#[derive(Debug, Default)]
struct PendingState {
    closed: bool,
    calls: HashMap<u64, oneshot::Sender<Result<Value, RpcFailure>>>,
}

type Pending = Arc<Mutex<PendingState>>;

#[derive(Debug)]
enum RpcFailure {
    Remote { code: i64, message: String },
    Closed,
}

enum Outgoing {
    Line(String),
    /// Close stdin.
    Close,
}

/// The sending half of the connection; cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct Transport {
    outgoing: mpsc::Sender<Outgoing>,
    pending: Pending,
    next_id: AtomicU64,
    kill: mpsc::Sender<()>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Transport {
    /// Starts the process and its reader, writer and reaper tasks.
    pub fn spawn(launch: &Launch) -> Result<Spawned, AgentError> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &launch.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|source| AgentError::Spawn {
            program: launch.program.display().to_string(),
            source,
        })?;
        // `Stdio::piped()` was requested for all three, so the handles exist.
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AgentError::Protocol("child pipes missing".to_owned()));
        };

        let (outgoing, outgoing_rx) = mpsc::channel(OUTGOING_QUEUE);
        let (incoming_tx, incoming) = mpsc::channel(INCOMING_QUEUE);
        let (kill, kill_rx) = mpsc::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        let pending: Pending = Arc::default();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));

        tokio::spawn(write_loop(stdin, outgoing_rx));
        tokio::spawn(read_loop(stdout, pending.clone(), incoming_tx));
        tokio::spawn(stderr_loop(stderr, stderr_tail.clone()));
        tokio::spawn(reap(child, kill_rx, exit_tx, stderr_tail.clone()));

        Ok((
            Arc::new(Self {
                outgoing,
                pending,
                next_id: AtomicU64::new(1),
                kill,
                stderr: stderr_tail,
            }),
            incoming,
            exit_rx,
        ))
    }

    /// The last lines of the process's stderr, newest last.
    pub fn stderr_tail(&self) -> String {
        lock(&self.stderr)
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn send_line(&self, value: &Value) -> Result<(), AgentError> {
        let line = value.to_string();
        self.outgoing
            .send(Outgoing::Line(line))
            .await
            .map_err(|_| self.exited())
    }

    fn exited(&self) -> AgentError {
        AgentError::Exited {
            detail: self.stderr_tail(),
        }
    }

    /// A request; resolves with the response's `result`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AgentError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = lock(&self.pending);
            if pending.closed {
                return Err(self.exited());
            }
            pending.calls.insert(id, tx);
        }
        let message = json!({"method": method, "id": id, "params": params});
        if let Err(error) = self.send_line(&message).await {
            lock(&self.pending).calls.remove(&id);
            return Err(error);
        }
        match timeout(REQUEST_TIMEOUT, rx).await {
            Err(_) => {
                lock(&self.pending).calls.remove(&id);
                Err(AgentError::Timeout {
                    method: method.to_owned(),
                })
            }
            Ok(Err(_) | Ok(Err(RpcFailure::Closed))) => Err(self.exited()),
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(RpcFailure::Remote { code, message }))) => Err(AgentError::Rpc {
                method: method.to_owned(),
                code,
                message,
            }),
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), AgentError> {
        let message = match params {
            Some(params) => json!({"method": method, "params": params}),
            None => json!({"method": method}),
        };
        self.send_line(&message).await
    }

    /// Answers a request of the process.
    pub async fn respond(&self, id: &Value, result: Value) -> Result<(), AgentError> {
        self.send_line(&json!({"id": id, "result": result})).await
    }

    pub async fn respond_error(
        &self,
        id: &Value,
        code: i64,
        message: &str,
    ) -> Result<(), AgentError> {
        self.send_line(&json!({"id": id, "error": {"code": code, "message": message}}))
            .await
    }

    /// Closes stdin, then kills the process if it is still there after a short grace. The
    /// output ends, which the consumer sees as [`Incoming::Closed`].
    pub async fn shutdown(&self) {
        // Failure means the process is already gone.
        let _ = self.outgoing.send(Outgoing::Close).await;
        let _ = self.kill.send(()).await;
    }
}

async fn write_loop(mut stdin: ChildStdin, mut outgoing: mpsc::Receiver<Outgoing>) {
    while let Some(message) = outgoing.recv().await {
        match message {
            Outgoing::Line(mut line) => {
                line.push('\n');
                if let Err(error) = stdin.write_all(line.as_bytes()).await {
                    debug!(%error, "agent stdin closed");
                    return;
                }
                if let Err(error) = stdin.flush().await {
                    debug!(%error, "agent stdin closed");
                    return;
                }
            }
            Outgoing::Close => return,
        }
    }
}

async fn read_loop(stdout: ChildStdout, pending: Pending, incoming: mpsc::Sender<Incoming>) {
    let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    while let Some(line) = lines.next().await {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(%error, "unreadable line from the agent");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut message) = serde_json::from_str::<Value>(&line) else {
            warn!("agent sent a line that is not JSON");
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                let params = message
                    .get_mut("params")
                    .map(Value::take)
                    .unwrap_or_default();
                if incoming
                    .send(Incoming::Request { id, method, params })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            (Some(method), None) => {
                let params = message
                    .get_mut("params")
                    .map(Value::take)
                    .unwrap_or_default();
                if incoming
                    .send(Incoming::Notification { method, params })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            (None, Some(id)) => resolve(&pending, &id, &mut message),
            (None, None) => warn!("agent sent a message without method or id"),
        }
    }
    // Requests still waiting will never be answered.
    let waiting: Vec<_> = {
        let mut pending = lock(&pending);
        pending.closed = true;
        pending.calls.drain().collect()
    };
    for (_, tx) in waiting {
        let _ = tx.send(Err(RpcFailure::Closed));
    }
    let _ = incoming.send(Incoming::Closed).await;
}

fn resolve(pending: &Pending, id: &Value, message: &mut Value) {
    let Some(id) = id.as_u64() else {
        warn!("agent answered with an id we never sent");
        return;
    };
    let Some(tx) = lock(pending).calls.remove(&id) else {
        debug!(id, "response to a request nobody waits for");
        return;
    };
    let outcome = match message.get_mut("error") {
        Some(error) => Err(RpcFailure::Remote {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned(),
        }),
        None => Ok(message
            .get_mut("result")
            .map(Value::take)
            .unwrap_or_default()),
    };
    let _ = tx.send(outcome);
}

async fn stderr_loop(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        debug!(target: "staffy_agents::stderr", "{line}");
        let mut tail = lock(&tail);
        if tail.len() == STDERR_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Owns the child: reports how it ended, and ends it when asked to.
async fn reap(
    mut child: Child,
    mut kill: mpsc::Receiver<()>,
    exit: oneshot::Sender<ExitInfo>,
    stderr: Arc<Mutex<VecDeque<String>>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        Some(()) = kill.recv() => {
            match timeout(EXIT_GRACE, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    if let Err(error) = child.start_kill() {
                        warn!(%error, "could not kill the agent process");
                    }
                    child.wait().await
                }
            }
        }
    };
    let (code, signal) = match status {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            (status.code(), status.signal())
        }
        Err(error) => {
            warn!(%error, "waiting for the agent process failed");
            (None, None)
        }
    };
    // Let the stderr task drain what the process wrote last.
    tokio::task::yield_now().await;
    let stderr_tail = lock(&stderr).iter().cloned().collect::<Vec<_>>().join("\n");
    let _ = exit.send(ExitInfo {
        code,
        signal,
        stderr_tail,
    });
}
