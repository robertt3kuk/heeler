//! Durable terminal attachments: [`TerminalHandle`] outlives individual connections.

use heeler_core::TerminalId;
use heeler_protocol::messages::TerminalSize;
use heeler_protocol::render::TerminalRender;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Renders buffered per handle. The daemon sends one frame per ack (credit 1), so a consumer
/// that acks only what it applied never has more than a couple pending.
pub(crate) const UPDATE_BUFFER: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum TerminalUpdate {
    /// A snapshot or frame. Every (re)attach starts with a snapshot.
    Render(TerminalRender),
    /// The connection dropped; the client re-attaches by itself, the next render is a snapshot.
    Reconnecting,
    /// The terminal can no longer be shown through this handle; no update follows.
    Gone { reason: GoneReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoneReason {
    /// The daemon restarted (new epoch): all terminals of the old daemon are gone.
    EpochChanged,
    /// The daemon does not know the terminal (its workspace was closed, or the id is wrong).
    NotFound,
    /// The session restarted and continues in `new_terminal`.
    SessionRestarted { new_terminal: TerminalId },
}

/// Requests from handles to the connection task.
#[derive(Debug)]
pub(crate) enum Command {
    Attach {
        handle: u64,
        terminal: TerminalId,
        size: Option<TerminalSize>,
        updates: mpsc::Sender<TerminalUpdate>,
    },
    Detach {
        handle: u64,
        terminal: TerminalId,
    },
    Ack {
        handle: u64,
        terminal: TerminalId,
        frame: u64,
    },
    Input {
        handle: u64,
        terminal: TerminalId,
        bytes: Vec<u8>,
    },
    Resize {
        handle: u64,
        terminal: TerminalId,
        size: TerminalSize,
    },
}

impl Command {
    /// Name for logs; never includes payload bytes (input may be a password).
    fn kind(&self) -> &'static str {
        match self {
            Command::Attach { .. } => "attach",
            Command::Detach { .. } => "detach",
            Command::Ack { .. } => "ack",
            Command::Input { .. } => "input",
            Command::Resize { .. } => "resize",
        }
    }
}

/// A live view of one terminal. Dropping the handle detaches it.
///
/// At most one handle per terminal is active per [`Connection`](crate::Connection): attaching
/// again replaces the earlier handle, whose update stream then ends.
#[derive(Debug)]
pub struct TerminalHandle {
    terminal: TerminalId,
    id: u64,
    updates: mpsc::Receiver<TerminalUpdate>,
    commands: mpsc::Sender<Command>,
}

impl TerminalHandle {
    pub(crate) fn new(
        terminal: TerminalId,
        id: u64,
        updates: mpsc::Receiver<TerminalUpdate>,
        commands: mpsc::Sender<Command>,
    ) -> Self {
        Self {
            terminal,
            id,
            updates,
            commands,
        }
    }

    pub fn terminal(&self) -> TerminalId {
        self.terminal
    }

    /// Updates in order. Bounded; `recv` yields `None` after [`TerminalUpdate::Gone`] or when
    /// the connection was shut down.
    pub fn updates(&mut self) -> &mut mpsc::Receiver<TerminalUpdate> {
        &mut self.updates
    }

    /// Grants the daemon credit for the next render by acknowledging `frame`, the frame number
    /// of the render just applied. Fire-and-forget; ignored while disconnected (the re-attach
    /// starts over with a snapshot).
    pub fn ack(&self, frame: u64) {
        self.send(Command::Ack {
            handle: self.id,
            terminal: self.terminal,
            frame,
        });
    }

    /// Sends bytes to the terminal's PTY. Fire-and-forget; dropped while disconnected.
    pub fn input(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.send(Command::Input {
            handle: self.id,
            terminal: self.terminal,
            bytes,
        });
    }

    /// Resizes the terminal. The size is remembered and sent again with the next re-attach.
    pub fn resize(&self, size: TerminalSize) {
        self.send(Command::Resize {
            handle: self.id,
            terminal: self.terminal,
            size,
        });
    }

    fn send(&self, command: Command) {
        send_command(&self.commands, command);
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        send_command(
            &self.commands,
            Command::Detach {
                handle: self.id,
                terminal: self.terminal,
            },
        );
    }
}

/// Queues `command` without waiting. A closed queue means the connection is gone (nothing to
/// do); a full one means the connection task is not keeping up and the command is lost.
pub(crate) fn send_command(commands: &mpsc::Sender<Command>, command: Command) {
    match commands.try_send(command) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(command)) => {
            warn!(
                command = command.kind(),
                "connection task is behind; dropping a terminal command"
            );
        }
        Err(mpsc::error::TrySendError::Closed(command)) => {
            debug!(
                command = command.kind(),
                "connection is gone; terminal command ignored"
            );
        }
    }
}
