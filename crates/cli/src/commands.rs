//! The commands behind `heeler`'s subcommands.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use heeler_client::{Client, ClientError, ConnectOptions, default_socket_path};
use heeler_core::WorkspaceId;
use heeler_protocol::ErrorBody;
use heeler_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Event, Request,
    SessionSnapshot, Subscribed, WorkspaceCreate, WorkspaceInfo, WorkspaceList, WorkspaceRef,
    WorkspaceRename,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::sleep;

use crate::args::{ApiArgs, Cli, Command, DaemonCommand, EventsArgs, WsCommand};
use crate::error::{CliError, is_unreachable};

const STOP_WAIT: Duration = Duration::from_secs(10);
const STOP_POLL: Duration = Duration::from_millis(25);

pub async fn run(cli: Cli) -> Result<(), CliError> {
    let ctx = Ctx::new(&cli)?;
    match cli.command {
        Command::Daemon(DaemonCommand::Start) => ctx.daemon_start().await,
        Command::Daemon(DaemonCommand::Status) => ctx.daemon_status().await,
        Command::Daemon(DaemonCommand::Stop { stop_sessions }) => {
            ctx.daemon_stop(stop_sessions).await
        }
        Command::Ws(WsCommand::Ls) => ctx.ws_ls().await,
        Command::Ws(WsCommand::New { name, roots }) => ctx.ws_new(name, roots).await,
        Command::Ws(WsCommand::Rename { id, name }) => ctx.ws_rename(id, name).await,
        Command::Ws(WsCommand::Close { id }) => ctx.ws_close(id).await,
        Command::Api(args) => ctx.api(args).await,
        Command::Events(args) => ctx.events(args).await,
    }
}

struct Ctx {
    /// `--socket` as given; `None` lets the client resolve it from the environment.
    socket_arg: Option<PathBuf>,
    /// The socket that will be dialed, for messages.
    socket: PathBuf,
    json: bool,
}

impl Ctx {
    fn new(cli: &Cli) -> Result<Self, CliError> {
        let socket = cli
            .socket
            .clone()
            .or_else(default_socket_path)
            .ok_or_else(|| CliError::from_client(ClientError::NoSocketPath, Path::new("")))?;
        Ok(Self {
            socket_arg: cli.socket.clone(),
            socket,
            json: cli.json,
        })
    }

    fn options(&self, autostart: Option<PathBuf>) -> ConnectOptions {
        ConnectOptions {
            socket: self.socket_arg.clone(),
            autostart,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        }
    }

    fn fail(&self, error: ClientError) -> CliError {
        CliError::from_client(error, &self.socket)
    }

    async fn connect(&self) -> Result<Client, CliError> {
        Client::connect(self.options(None))
            .await
            .map_err(|e| self.fail(e))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        client: &Client,
        request: Request,
    ) -> Result<T, CliError> {
        client.call(request).await.map_err(|e| self.fail(e))
    }

    /// JSON when `--json`, otherwise the human rendering.
    fn print(
        &self,
        value: &impl Serialize,
        human: impl FnOnce() -> String,
    ) -> Result<(), CliError> {
        if self.json {
            print_line(&serde_json::to_string(value).map_err(io_error)?)
        } else {
            print_line(&human())
        }
    }

    // ---- daemon ----------------------------------------------------------------------------

    async fn daemon_start(&self) -> Result<(), CliError> {
        let (client, started) = match Client::connect(self.options(None)).await {
            Ok(client) => (client, false),
            Err(ClientError::Io(e)) if is_unreachable(&e) => {
                let binary = find_heelerd()?;
                let client = Client::connect(self.options(Some(binary)))
                    .await
                    .map_err(|e| self.fail(e))?;
                (client, true)
            }
            Err(e) => return Err(self.fail(e)),
        };
        let info: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        let out = StartOut {
            started,
            status: StatusOut::new(&self.socket, info),
        };
        self.print(&out, || {
            let verb = if started {
                "started"
            } else {
                "already running"
            };
            format!("{verb}: {}", out.status.human())
        })
    }

    async fn daemon_status(&self) -> Result<(), CliError> {
        let client = match self.connect().await {
            Ok(client) => client,
            Err(error @ CliError::Unreachable(_)) => {
                if self.json {
                    print_line(
                        &serde_json::json!({"running": false, "socket": self.socket}).to_string(),
                    )?;
                }
                return Err(error);
            }
            Err(e) => return Err(e),
        };
        let info: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        let out = StatusOut::new(&self.socket, info);
        self.print(&out, || out.human())
    }

    async fn daemon_stop(&self, stop_sessions: bool) -> Result<(), CliError> {
        let client = self.connect().await?;
        match client
            .call::<Value>(Request::DaemonShutdown(DaemonShutdown { stop_sessions }))
            .await
        {
            Ok(_) | Err(ClientError::Disconnected) => {}
            Err(e) => return Err(self.fail(e)),
        }
        drop(client);
        // The daemon removes its socket just before releasing its lock, so once the file is
        // gone `daemon start` can succeed.
        let gone = tokio::time::timeout(STOP_WAIT, async {
            while std::fs::symlink_metadata(&self.socket).is_ok() {
                sleep(STOP_POLL).await;
            }
        })
        .await;
        if gone.is_err() {
            return Err(CliError::Failed(format!(
                "heelerd accepted the shutdown but {} is still there after {}s",
                self.socket.display(),
                STOP_WAIT.as_secs()
            )));
        }
        self.print(&serde_json::json!({"stopped": true}), || {
            "heelerd stopped".to_owned()
        })
    }

    // ---- workspaces ------------------------------------------------------------------------

    async fn ws_ls(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let list: WorkspaceList = self.call(&client, Request::WorkspaceList(Empty {})).await?;
        self.print(&list, || {
            if list.workspaces.is_empty() {
                return "no workspaces".to_owned();
            }
            let width = list
                .workspaces
                .iter()
                .map(|ws| ws.name.chars().count())
                .max();
            let width = width.unwrap_or(0).max("NAME".len());
            let mut table = format!("{:<26}  {:<width$}  ROOTS", "ID", "NAME");
            for ws in &list.workspaces {
                let roots: Vec<_> = ws.roots.iter().map(|r| r.display().to_string()).collect();
                table.push_str(&format!(
                    "\n{}  {:<width$}  {}",
                    ws.id,
                    ws.name,
                    roots.join(", ")
                ));
            }
            table
        })
    }

    async fn ws_new(&self, name: String, roots: Vec<PathBuf>) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let roots = roots
            .iter()
            .map(std::path::absolute)
            .collect::<Result<Vec<_>, _>>()?;
        let client = self.connect().await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceCreate(WorkspaceCreate { name, roots }),
            )
            .await?;
        self.print(&ws, || format!("created {} ({})", ws.id, ws.name))
    }

    async fn ws_rename(&self, id: WorkspaceId, name: String) -> Result<(), CliError> {
        let client = self.connect().await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceRename(WorkspaceRename {
                    workspace_id: id,
                    name,
                }),
            )
            .await?;
        self.print(&ws, || format!("renamed {} to {}", ws.id, ws.name))
    }

    async fn ws_close(&self, id: WorkspaceId) -> Result<(), CliError> {
        let client = self.connect().await?;
        let _: Value = self
            .call(
                &client,
                Request::WorkspaceClose(WorkspaceRef { workspace_id: id }),
            )
            .await?;
        self.print(
            &serde_json::json!({"workspace_id": id, "closed": true}),
            || format!("closed {id}"),
        )
    }

    // ---- raw API and events ------------------------------------------------------------------

    async fn api(&self, args: ApiArgs) -> Result<(), CliError> {
        let params = match args.params.as_deref() {
            Some(text) => serde_json::from_str(text)
                .map_err(|e| CliError::Usage(format!("params are not valid JSON: {e}")))?,
            None => Value::Object(serde_json::Map::new()),
        };
        // A request the typed protocol cannot express never reaches the wire; it gets the
        // same error body the daemon would answer with.
        let outcome = match Request::decode(&args.method, params) {
            Ok(request) => {
                let client = self.connect().await?;
                match client.call::<Value>(request).await {
                    Ok(value) => Ok(value),
                    Err(ClientError::Remote(body)) => Err(body),
                    Err(e) => return Err(self.fail(e)),
                }
            }
            Err(body) => Err(body),
        };
        match outcome {
            Ok(value) => print_line(&json_text(&value, self.json)?),
            Err(body) => {
                print_line(&json_text(&error_value(&body), self.json)?)?;
                Err(CliError::Reported)
            }
        }
    }

    async fn events(&self, args: EventsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        // Subscribe before the snapshot so nothing between them is lost; events at or below
        // the snapshot's seq are already part of it.
        let mut stream = args.follow.then(|| client.events());
        if args.follow {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let snapshot: SessionSnapshot = self
            .call(&client, Request::SessionSnapshot(Empty {}))
            .await?;
        let epoch = client.epoch();
        let line = |seq: u64, event: &Event| -> Result<String, CliError> {
            let (name, data) = event
                .encode()
                .map_err(|e| CliError::Failed(e.to_string()))?;
            serde_json::to_string(&serde_json::json!({
                "epoch": epoch,
                "seq": seq,
                "event": name,
                "data": data,
            }))
            .map_err(io_error)
        };
        for workspace in snapshot.workspaces {
            print_line(&line(
                snapshot.seq.0,
                &Event::WorkspaceUpdated { workspace },
            )?)?;
        }
        for session in snapshot.sessions {
            print_line(&line(snapshot.seq.0, &Event::SessionUpdated { session })?)?;
        }
        let Some(stream) = stream.as_mut() else {
            return Ok(());
        };
        loop {
            match stream.recv().await {
                Ok((seq, event)) if seq > snapshot.seq => print_line(&line(seq.0, &event)?)?,
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    return Err(CliError::Failed(format!(
                        "fell behind the event stream by {missed} events; rerun to resync"
                    )));
                }
                Err(RecvError::Closed) => return Err(self.fail(ClientError::Disconnected)),
            }
        }
    }
}

#[derive(Serialize)]
struct StatusOut {
    running: bool,
    socket: PathBuf,
    #[serde(flatten)]
    info: DaemonStatusInfo,
}

impl StatusOut {
    fn new(socket: &Path, info: DaemonStatusInfo) -> Self {
        Self {
            running: true,
            socket: socket.to_owned(),
            info,
        }
    }

    fn human(&self) -> String {
        let i = &self.info;
        format!(
            "heelerd {} (protocol {}.{}) pid {}, up {}, {} session(s), {} client(s)\nsocket {}\nepoch  {}",
            i.daemon_version,
            i.protocol.major,
            i.protocol.minor,
            i.pid,
            format_uptime(i.uptime_ms / 1000),
            i.sessions,
            i.clients,
            self.socket.display(),
            i.epoch,
        )
    }
}

#[derive(Serialize)]
struct StartOut {
    started: bool,
    #[serde(flatten)]
    status: StatusOut,
}

fn format_uptime(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{}s", secs / 60, secs % 60),
        _ => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
    }
}

fn error_value(body: &ErrorBody) -> Value {
    serde_json::to_value(body).unwrap_or(Value::Null)
}

fn json_text(value: &Value, compact: bool) -> Result<String, CliError> {
    if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    }
    .map_err(io_error)
}

fn io_error(e: serde_json::Error) -> CliError {
    CliError::Failed(format!("encoding output: {e}"))
}

fn print_line(text: &str) -> Result<(), CliError> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}")?;
    Ok(())
}

/// `heelerd` next to this binary (also through symlinks), else the first one on `PATH`.
fn find_heelerd() -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe().ok();
    let beside = exe
        .iter()
        .flat_map(|exe| [Some(exe.clone()), exe.canonicalize().ok()])
        .flatten()
        .filter_map(|exe| exe.parent().map(|dir| dir.join("heelerd")));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("heelerd"));
    beside
        .chain(on_path)
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| {
            CliError::Failed("heelerd not found next to the heeler binary or on PATH".to_owned())
        })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
