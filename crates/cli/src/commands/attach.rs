//! `heeler attach`: this terminal becomes a client of one daemon terminal.
//!
//! The daemon renders; this side only draws its snapshots and frames onto the host screen
//! (acking each one after it was written, which is what lets the daemon send the next) and
//! forwards raw keystrokes back. See `docs/protocol.md` for the render stream and ADR 0004.

use std::io::{self, Read as _, Write as _};
use std::time::Duration;

use heeler_client::{Client, ClientError};
use heeler_core::{Seq, SessionId, SessionState, TerminalId};
use heeler_protocol::messages::{
    Empty, Event, Request, SessionInfo, Subscribed, TerminalAck, TerminalAttach, TerminalAttached,
    TerminalRef, TerminalResize, TerminalSize,
};
use heeler_protocol::render::TerminalRender;
use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Instant, sleep_until};

use super::sessions::{live_terminal, require_tty, terminal_size};
use super::{Ctx, print_line};
use crate::error::CliError;
use heeler_cli::host::{self, RawMode};
use heeler_cli::{DetachDetector, HostRenderer};

/// After the session ended, how long to wait for a restart (a new live terminal id) before
/// leaving. `session.updated` for the end and for a restart are separate events.
const RESTART_GRACE: Duration = Duration::from_millis(300);
/// Host input reads queued between the stdin thread and the attach loop.
const INPUT_QUEUE: usize = 16;

/// How an attach ended without an error.
#[derive(Debug)]
enum Outcome {
    Detached,
    /// The session ended; the text is the status line to print.
    Ended(String),
}

impl Ctx {
    pub(super) async fn attach(&self, target: &str) -> Result<(), CliError> {
        require_tty()?;
        let client = self.connect().await?;
        // Subscribe before looking the session up so its end cannot slip between the two.
        let events = client.events();
        let _: Subscribed = self
            .call(&client, Request::EventsSubscribe(Empty {}))
            .await?;
        let session = self.resolve(&client, target).await?;
        if session.state.is_live() {
            self.attach_session(client, events, session).await
        } else {
            self.show_final_screen(&client, &session).await
        }
    }

    /// Attaching to an ended session: its terminal stays attachable, so draw the final screen
    /// inside the usual enter/leave, then leave at once and print how the session ended.
    async fn show_final_screen(
        &self,
        client: &Client,
        session: &SessionInfo,
    ) -> Result<(), CliError> {
        let terminal = session
            .terminal_id
            .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))?;
        let mut signals = Signals::new()?;
        let mut screen = HostScreen::enter()?;
        let drawn = tokio::select! {
            drawn = self.draw_final_screen(client, terminal, &mut screen) => drawn,
            _ = signals.term.recv() => Err(signalled("SIGTERM", 15)),
            _ = signals.hup.recv() => Err(signalled("SIGHUP", 1)),
            _ = signals.int.recv() => Err(signalled("SIGINT", 2)),
        };
        screen.leave();
        drop(screen);
        drawn?;
        print_line(&end_message(&session.state))?;
        print_line(&format!(
            "`heeler restart {id}` starts it again, `heeler history {id}` shows its output",
            id = session.id
        ))
    }

    /// Draws the snapshot the daemon queues before it answers the attach.
    async fn draw_final_screen(
        &self,
        client: &Client,
        terminal: TerminalId,
        screen: &mut HostScreen,
    ) -> Result<(), CliError> {
        let mut frames = self.open_terminal(client, terminal).await?;
        let render = frames
            .recv()
            .await
            .ok_or_else(|| self.fail(ClientError::Disconnected))?;
        let frame = screen.draw(&render)?;
        let _: Value = self
            .call(
                client,
                Request::TerminalAck(TerminalAck {
                    terminal_id: terminal,
                    frame,
                }),
            )
            .await?;
        Ok(())
    }

    /// Runs the attach loop for a live `session`; `events` must have been subscribed before the
    /// session was looked up or created.
    pub(super) async fn attach_session(
        &self,
        client: Client,
        mut events: broadcast::Receiver<(Seq, Event)>,
        session: SessionInfo,
    ) -> Result<(), CliError> {
        let id = session.id;
        let terminal = live_terminal(&session)?;
        let mut signals = Signals::new()?;
        let mut input = spawn_stdin()?;
        let mut screen = HostScreen::enter()?;

        let outcome = self
            .attach_loop(
                &client,
                &mut events,
                id,
                terminal,
                &mut screen,
                &mut input,
                &mut signals,
            )
            .await;

        // The terminal is back to normal before anything is printed (also when `outcome` is an
        // error: `main` prints it after this returns).
        screen.leave();
        drop(screen);
        match outcome? {
            Outcome::Detached => print_line(&format!("[detached from {id}]")),
            Outcome::Ended(message) => print_line(&message),
        }
    }

    #[allow(clippy::too_many_arguments)] // one loop, borrowed pieces of one attach
    async fn attach_loop(
        &self,
        client: &Client,
        events: &mut broadcast::Receiver<(Seq, Event)>,
        id: SessionId,
        mut terminal: TerminalId,
        screen: &mut HostScreen,
        input: &mut mpsc::Receiver<Vec<u8>>,
        signals: &mut Signals,
    ) -> Result<Outcome, CliError> {
        let mut frames = self.open_terminal(client, terminal).await?;
        let mut detector = DetachDetector::new();
        let mut forward = Vec::new();
        // Set while the session is over and a restart could still follow.
        let mut ended: Option<(Instant, String)> = None;

        loop {
            let leave_at = ended
                .as_ref()
                .map_or_else(|| Instant::now() + Duration::from_secs(3600), |(at, _)| *at);
            tokio::select! {
                render = frames.recv() => {
                    let render = render.ok_or_else(|| self.fail(ClientError::Disconnected))?;
                    let frame = screen.draw(&render)?;
                    // Only now may the daemon send the next render.
                    let _: Value = self
                        .call(client, Request::TerminalAck(TerminalAck { terminal_id: terminal, frame }))
                        .await?;
                }
                keys = input.recv() => {
                    // A closed stdin is a vanished terminal: leave like a detach.
                    let mut detach = keys.is_none();
                    if let Some(keys) = keys {
                        forward.clear();
                        detach = detector.feed(&keys, &mut forward);
                        if !forward.is_empty() {
                            client.send_input(terminal, &forward).await.map_err(|e| self.fail(e))?;
                        }
                    }
                    if detach {
                        // Best effort: the daemon drops the attachment with the connection anyway.
                        let _ = client
                            .call::<Value>(Request::TerminalDetach(TerminalRef { terminal_id: terminal }))
                            .await;
                        return Ok(Outcome::Detached);
                    }
                }
                _ = signals.winch.recv() => {
                    let (cols, rows) = host::size()?;
                    screen.resize(cols, rows)?;
                    let _: Value = self
                        .call(client, Request::TerminalResize(TerminalResize {
                            terminal_id: terminal,
                            size: terminal_size(cols, rows),
                        }))
                        .await?;
                }
                event = events.recv() => {
                    let session = match event {
                        Ok((_, Event::SessionUpdated { session })) if session.id == id => session,
                        Ok(_) => continue,
                        Err(RecvError::Lagged(_)) => {
                            // Events were lost; the list has the state they led to.
                            let list = self.session_list(client, None).await?;
                            match list.sessions.into_iter().find(|s| s.id == id) {
                                Some(session) => session,
                                None => return Ok(Outcome::Ended("[session is gone]".to_owned())),
                            }
                        }
                        Err(RecvError::Closed) => return Err(self.fail(ClientError::Disconnected)),
                    };
                    if let Some(next) = session_step(&session, terminal, &mut ended) {
                        // Restarted: same session, new terminal.
                        terminal = next;
                        frames = self.open_terminal(client, terminal).await?;
                    }
                }
                () = sleep_until(leave_at), if ended.is_some() => {
                    if let Some((_, message)) = ended.take() {
                        return Ok(Outcome::Ended(message));
                    }
                }
                _ = signals.term.recv() => return Err(signalled("SIGTERM", 15)),
                _ = signals.hup.recv() => return Err(signalled("SIGHUP", 1)),
                _ = signals.int.recv() => return Err(signalled("SIGINT", 2)),
            }
        }
    }

    /// Attaches at the host's current size. The sink is registered first because the daemon
    /// queues the snapshot before it answers.
    async fn open_terminal(
        &self,
        client: &Client,
        terminal: TerminalId,
    ) -> Result<mpsc::Receiver<TerminalRender>, CliError> {
        let (cols, rows) = host::size()?;
        let frames = client.terminal_frames(terminal);
        let _: TerminalAttached = self
            .call(
                client,
                Request::TerminalAttach(TerminalAttach {
                    terminal_id: terminal,
                    size: Some(TerminalSize {
                        cols,
                        rows,
                        pixel_width: 0,
                        pixel_height: 0,
                    }),
                }),
            )
            .await?;
        Ok(frames)
    }
}

/// Folds a `session.updated` for the attached session into the loop state. Returns the new
/// terminal when the session is live on a different one (a restart).
fn session_step(
    session: &SessionInfo,
    current: TerminalId,
    ended: &mut Option<(Instant, String)>,
) -> Option<TerminalId> {
    if session.state.is_live() {
        *ended = None;
        return session.terminal_id.filter(|terminal| *terminal != current);
    }
    let deadline = ended
        .as_ref()
        .map_or_else(|| Instant::now() + RESTART_GRACE, |(at, _)| *at);
    *ended = Some((deadline, end_message(&session.state)));
    None
}

/// The status line printed when a session ends under an attached client.
pub(super) fn end_message(state: &SessionState) -> String {
    match state {
        SessionState::Exited {
            code: Some(code), ..
        } => format!("[session exited with code {code}]"),
        SessionState::Exited {
            code: None,
            signal: Some(signal),
        } => format!("[session killed by signal {signal}]"),
        SessionState::Exited { .. } => "[session exited]".to_owned(),
        SessionState::Stopped => "[session stopped]".to_owned(),
        SessionState::Failed { reason } => format!("[session failed: {reason}]"),
        _ => "[session ended]".to_owned(),
    }
}

fn signalled(name: &'static str, signal: u8) -> CliError {
    CliError::Signalled {
        name,
        code: 128 + signal,
    }
}

/// Signals that end or reshape an attach. Installed before raw mode so none is missed.
#[derive(Debug)]
struct Signals {
    winch: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> io::Result<Self> {
        Ok(Self {
            winch: signal(SignalKind::window_change())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
            int: signal(SignalKind::interrupt())?,
        })
    }
}

/// The host terminal while attached: raw mode plus the renderer's alternate screen. Dropping it
/// (also while unwinding from a panic) gives the screen and the terminal modes back.
#[derive(Debug)]
struct HostScreen {
    renderer: HostRenderer,
    entered: bool,
    /// Reused buffer for the bytes of one render.
    scratch: Vec<u8>,
    /// Declared last: restored after the leave sequence was written by `Drop`.
    _raw: RawMode,
}

impl HostScreen {
    fn enter() -> io::Result<Self> {
        let (cols, rows) = host::size()?;
        let raw = RawMode::enable()?;
        let screen = Self {
            renderer: HostRenderer::new(cols, rows),
            entered: true,
            scratch: Vec::new(),
            _raw: raw,
        };
        let mut out = Vec::new();
        screen.renderer.enter(&mut out);
        screen.write(&out)?;
        Ok(screen)
    }

    /// Writes one render to the host screen; returns its frame number for the ack.
    fn draw(&mut self, render: &TerminalRender) -> io::Result<u64> {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let frame = match render {
            TerminalRender::Snapshot(snapshot) => {
                self.renderer.apply_snapshot(snapshot, &mut out);
                snapshot.frame
            }
            TerminalRender::Frame(frame) => {
                self.renderer.apply_frame(frame, &mut out);
                frame.frame
            }
        };
        let written = self.write(&out);
        self.scratch = out;
        written?;
        Ok(frame)
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let mut out = Vec::new();
        self.renderer.resize_host(cols, rows, &mut out);
        self.write(&out)
    }

    fn leave(&mut self) {
        if std::mem::take(&mut self.entered) {
            let mut out = Vec::new();
            self.renderer.leave(&mut out);
            // Nothing useful to do if the terminal is gone.
            let _ = self.write(&out);
        }
    }
}

impl Drop for HostScreen {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Reads host input on its own thread: a blocking read on a terminal cannot be cancelled, and
/// the thread simply dies with the process. The channel closes when stdin does.
fn spawn_stdin() -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let (tx, rx) = mpsc::channel(INPUT_QUEUE);
    std::thread::Builder::new()
        .name("heeler-stdin".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })?;
    Ok(rx)
}
