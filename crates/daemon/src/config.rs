//! `daemon.toml`: settings only the daemon reads.
//!
//! ```toml
//! [daemon]
//! restore_sessions = "respawn"   # or "stopped"
//!
//! [workspaces]
//! home_folder = "~/Notes"        # the Home workspace's folder; default ~/Heeler
//!
//! [history]                      # session history index, off unless enabled (docs/history-index.md)
//! enabled = false
//! range_days = 0                 # 0 = everything, or 30, or 90
//! heeler = true                  # per-source switches
//! claude = true
//! codex = true
//! omp = true
//!
//! [history.roots]                # override where transcripts live (default: the agents' own dirs)
//! claude = "/path/to/claude/projects"
//!
//! [codex]                        # chat sessions (protocol 0.9)
//! bin = "/Users/me/.local/bin/codex"   # default: `codex` on the daemon's PATH
//! args = ["app-server", "-c", 'model_reasoning_effort="low"']   # default: ["app-server"]
//!
//! [gateway]                      # direct model chat (protocol 0.11, docs/gateway.md)
//! max_context_chars = 200000     # oldest messages are left out of a request beyond this
//!
//! [assistant]                    # assistant chat (protocol 0.12, docs/assistant.md)
//! provider = "openrouter"        # provider and model of new assistants
//! model = "meta-llama/llama-3.3-70b-instruct"
//! context_budget_chars = 60000   # attached context per message, split over the attachments
//!
//! [assistant.context]            # what assistants of project workspaces may read
//! shell = true
//! agents = true
//! files = true
//! search = true
//!
//! [assistant.global_context]     # the global (Home) assistant; files are off by default
//! files = false
//!
//! [assistant.workspaces.heeler]  # per workspace (name or id): only what differs
//! files = false
//! ```

use std::io;
use std::path::{Path, PathBuf};

use heeler_protocol::assistant::AttachmentKind;
use serde::Deserialize;
use serde::de::Error as _;

use crate::error::DaemonError;

/// What the daemon does with sessions that were alive when it stopped (ADR 0009 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Spawn them again with the same ids, titles, and workspaces, in their last known cwd.
    #[default]
    Respawn,
    /// List them as stopped; the user restarts what they want.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DaemonSettings {
    pub restore_sessions: RestoreMode,
    /// Absolute folder of the Home workspace; `None` uses the default
    /// ([`Paths::default_home_folder`](crate::Paths)).
    pub home_folder: Option<PathBuf>,
    pub history: HistorySettings,
    pub codex: CodexSettings,
    pub gateway: GatewaySettings,
    pub assistant: AssistantSettings,
}

/// `gateway.max_context_chars` when the setting is absent.
pub const DEFAULT_MAX_CONTEXT_CHARS: usize = 200_000;

/// `[gateway]` of `daemon.toml`: direct model chat.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GatewaySettings {
    /// The most characters of conversation one request carries: the oldest messages that do not
    /// fit are left out (and the turn says so). The system prompt does not count.
    pub max_context_chars: usize,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            max_context_chars: DEFAULT_MAX_CONTEXT_CHARS,
        }
    }
}

/// `[codex]` of `daemon.toml`: how chat sessions start `codex app-server`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct CodexSettings {
    /// The Codex executable; `None` looks for `codex` on `PATH`.
    pub bin: Option<PathBuf>,
    /// The arguments of the executable, replacing the default `["app-server"]` when not empty
    /// (`["app-server", "-c", "key=value"]` adds overrides).
    pub args: Vec<String>,
}

impl CodexSettings {
    pub fn to_config(&self) -> heeler_agents::CodexConfig {
        let mut config = heeler_agents::CodexConfig::default();
        if let Some(bin) = &self.bin {
            config.program.clone_from(bin);
        }
        if !self.args.is_empty() {
            config.args.clone_from(&self.args);
        }
        config
    }
}

/// Which source directories the history index reads; `None` = the agent's default location.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct HistoryRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub omp: Option<PathBuf>,
}

/// `[history]` of `daemon.toml`. Nothing is read from any transcript directory unless
/// `enabled` is true.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub enabled: bool,
    /// Index only the last N days: 0 (everything), 30, or 90.
    pub range_days: u32,
    pub heeler: bool,
    pub claude: bool,
    pub codex: bool,
    pub omp: bool,
    pub roots: HistoryRoots,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            range_days: 0,
            heeler: true,
            claude: true,
            codex: true,
            omp: true,
            roots: HistoryRoots::default(),
        }
    }
}

/// `assistant.context_budget_chars` when the setting is absent.
pub const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 60_000;

/// What an assistant may read when a message attaches it (`docs/assistant.md`). Nothing is read
/// that is not attached; these only decide whether an attachment is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContextPermissions {
    /// `shell_block`, `shell_recent`: terminal text.
    pub shell: bool,
    /// `agent_transcript`: the journal of a Heeler chat or an indexed transcript.
    pub agents: bool,
    /// `file`: files under the workspace roots.
    pub files: bool,
    /// `search`: the history index.
    pub search: bool,
}

impl Default for ContextPermissions {
    fn default() -> Self {
        Self {
            shell: true,
            agents: true,
            files: true,
            search: true,
        }
    }
}

impl ContextPermissions {
    /// The default of the global assistant: files are off (its folder is Home).
    pub fn global_default() -> Self {
        Self {
            files: false,
            ..Self::default()
        }
    }

    /// Whether an attachment of this kind is accepted.
    pub fn allows(self, kind: AttachmentKind) -> bool {
        match kind {
            AttachmentKind::ShellBlock | AttachmentKind::ShellRecent => self.shell,
            AttachmentKind::AgentTranscript => self.agents,
            AttachmentKind::File => self.files,
            AttachmentKind::Search => self.search,
        }
    }
}

/// The setting that gates an attachment kind, as `daemon.toml` names it.
pub fn gating_setting(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::ShellBlock | AttachmentKind::ShellRecent => "shell",
        AttachmentKind::AgentTranscript => "agents",
        AttachmentKind::File => "files",
        AttachmentKind::Search => "search",
    }
}

/// `[assistant.workspaces.<name or id>]`: the permissions of one workspace that differ from
/// `[assistant.context]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ContextOverride {
    pub shell: Option<bool>,
    pub agents: Option<bool>,
    pub files: Option<bool>,
    pub search: Option<bool>,
}

/// `[assistant]` of `daemon.toml`: the assistant sessions' model and what they may read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AssistantSettings {
    /// The provider (`heeler provider ls`) of assistants created from now on.
    pub provider: Option<String>,
    /// Their model; absent takes the provider's default model.
    pub model: Option<String>,
    /// Characters of attached context one message carries, split evenly over its attachments.
    pub context_budget_chars: usize,
    /// What assistants of project workspaces may read.
    pub context: ContextPermissions,
    /// What the global assistant (Home) may read.
    pub global_context: ContextPermissions,
    /// Differences per workspace, by workspace name or id.
    pub workspaces: std::collections::BTreeMap<String, ContextOverride>,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
            context: ContextPermissions::default(),
            global_context: ContextPermissions::global_default(),
            workspaces: std::collections::BTreeMap::new(),
        }
    }
}

impl AssistantSettings {
    /// The permissions of a project workspace: `[assistant.context]` with the overrides of
    /// `[assistant.workspaces.<id>]` and then `[assistant.workspaces.<name>]` applied.
    pub fn permissions_for_workspace(&self, name: &str, id: &str) -> ContextPermissions {
        let mut permissions = self.context;
        for key in [id, name] {
            if let Some(over) = self.workspaces.get(key) {
                permissions.shell = over.shell.unwrap_or(permissions.shell);
                permissions.agents = over.agents.unwrap_or(permissions.agents);
                permissions.files = over.files.unwrap_or(permissions.files);
                permissions.search = over.search.unwrap_or(permissions.search);
            }
        }
        permissions
    }
}

/// The `range_days` values the settings accept.
pub const RANGE_DAYS: [u32; 3] = [0, 30, 90];

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    daemon: DaemonTable,
    #[serde(default)]
    workspaces: WorkspacesTable,
    #[serde(default)]
    history: HistorySettings,
    #[serde(default)]
    codex: CodexSettings,
    #[serde(default)]
    gateway: GatewaySettings,
    #[serde(default)]
    assistant: AssistantSettings,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspacesTable {
    home_folder: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DaemonTable {
    #[serde(default)]
    restore_sessions: RestoreMode,
}

impl DaemonSettings {
    /// A missing file means defaults; an unreadable or invalid one is an error, so a typo
    /// does not silently change what happens to the user's sessions. `user_home` is what a
    /// leading `~` in `home_folder` stands for.
    pub fn load(path: &Path, user_home: Option<&Path>) -> Result<Self, DaemonError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(DaemonError::io(format!("reading {}", path.display()))(e)),
        };
        Self::parse(&text, user_home).map_err(|source| DaemonError::Config {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    fn parse(text: &str, user_home: Option<&Path>) -> Result<Self, toml::de::Error> {
        let file: File = toml::from_str(text)?;
        if !RANGE_DAYS.contains(&file.history.range_days) {
            return Err(toml::de::Error::custom(format!(
                "history.range_days must be 0, 30, or 90, not {}",
                file.history.range_days
            )));
        }
        if file.gateway.max_context_chars == 0 {
            return Err(toml::de::Error::custom(
                "gateway.max_context_chars must be at least 1",
            ));
        }
        let home_folder = file
            .workspaces
            .home_folder
            .map(|folder| expand_home_folder(&folder, user_home))
            .transpose()?;
        if file.assistant.context_budget_chars == 0 {
            return Err(toml::de::Error::custom(
                "assistant.context_budget_chars must be at least 1",
            ));
        }
        Ok(Self {
            restore_sessions: file.daemon.restore_sessions,
            home_folder,
            history: file.history,
            codex: file.codex,
            gateway: file.gateway,
            assistant: file.assistant,
        })
    }
}

/// `~` and `~/...` stand for the user's home directory; the result must be absolute.
fn expand_home_folder(folder: &str, user_home: Option<&Path>) -> Result<PathBuf, toml::de::Error> {
    let path = match folder.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = user_home.ok_or_else(|| {
                toml::de::Error::custom(
                    "workspaces.home_folder uses `~` but there is no home directory",
                )
            })?;
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(folder),
    };
    if !path.is_absolute() {
        return Err(toml::de::Error::custom(format!(
            "workspaces.home_folder must be an absolute path or start with `~/`, got `{folder}`"
        )));
    }
    Ok(path)
}

/// Records `[history] enabled` in `daemon.toml`, creating the file or the table when missing and
/// leaving every other key and comment as it is. Written to a temporary file first, so a crash
/// never leaves a half-written config.
pub fn set_history_enabled(path: &Path, enabled: bool) -> Result<(), DaemonError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let context = |what: &str| format!("{what} {}", path.display());
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(DaemonError::io(context("reading"))(e)),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        DaemonError::io(context("parsing"))(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    let history = doc
        .entry("history")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(table) = history.as_table_like_mut() else {
        return Err(DaemonError::io(context("updating"))(io::Error::new(
            io::ErrorKind::InvalidData,
            "`history` is not a table",
        )));
    };
    // Updating in place keeps the comments in front of the key.
    match table.get_mut("enabled") {
        Some(item) => *item = toml_edit::value(enabled),
        None => {
            table.insert("enabled", toml_edit::value(enabled));
        }
    }

    let tmp = path.with_extension("toml.tmp");
    let write = || -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(doc.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DaemonError::io(context("writing"))(e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_defaults_to_codex_on_path_with_app_server() {
        let config = DaemonSettings::parse("", None).unwrap().codex.to_config();
        assert_eq!(config.program, PathBuf::from("codex"));
        assert_eq!(config.args, ["app-server"]);
    }

    #[test]
    fn codex_settings_name_the_binary_and_extra_arguments() {
        let toml = "[codex]\nbin = \"/opt/codex\"\nargs = [\"app-server\", \"-c\", \"model_reasoning_effort=\\\"low\\\"\"]\n";
        let config = DaemonSettings::parse(toml, None).unwrap().codex.to_config();
        assert_eq!(config.program, PathBuf::from("/opt/codex"));
        assert_eq!(
            config.args,
            ["app-server", "-c", "model_reasoning_effort=\"low\""]
        );
    }

    #[test]
    fn the_gateway_context_limit_defaults_and_is_validated() {
        assert_eq!(
            DaemonSettings::parse("", None)
                .unwrap()
                .gateway
                .max_context_chars,
            200_000
        );
        let settings =
            DaemonSettings::parse("[gateway]\nmax_context_chars = 5000\n", None).unwrap();
        assert_eq!(settings.gateway.max_context_chars, 5000);
        assert!(DaemonSettings::parse("[gateway]\nmax_context_chars = 0\n", None).is_err());
    }

    #[test]
    fn defaults_to_respawn() {
        assert_eq!(
            DaemonSettings::parse("", None).unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert_eq!(
            DaemonSettings::parse("[daemon]\n", None)
                .unwrap()
                .restore_sessions,
            RestoreMode::Respawn
        );
    }

    #[test]
    fn reads_the_restore_mode() {
        let parse = |mode: &str| {
            DaemonSettings::parse(&format!("[daemon]\nrestore_sessions = \"{mode}\"\n"), None)
        };
        assert_eq!(
            parse("stopped").unwrap().restore_sessions,
            RestoreMode::Stopped
        );
        assert_eq!(
            parse("respawn").unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert!(parse("resurrect").is_err());
    }

    #[test]
    fn unrelated_tables_are_ignored() {
        let settings =
            DaemonSettings::parse("[other]\nx = 1\n[daemon]\nfuture = true\n", None).unwrap();
        assert_eq!(settings, DaemonSettings::default());
    }

    #[test]
    fn history_is_off_by_default_and_reads_its_table() {
        assert!(!DaemonSettings::parse("", None).unwrap().history.enabled);
        let settings = DaemonSettings::parse(
            "[history]\nenabled = true\nrange_days = 30\ncodex = false\n[history.roots]\nclaude = \"/tmp/c\"\n",
            None,
        )
        .unwrap()
        .history;
        assert!(settings.enabled && settings.heeler && settings.claude && settings.omp);
        assert!(!settings.codex);
        assert_eq!(settings.range_days, 30);
        assert_eq!(settings.roots.claude.as_deref(), Some(Path::new("/tmp/c")));
        assert_eq!(settings.roots.codex, None);
    }

    #[test]
    fn history_range_must_be_a_known_value() {
        for days in [0, 30, 90] {
            assert!(
                DaemonSettings::parse(&format!("[history]\nrange_days = {days}\n"), None).is_ok()
            );
        }
        let error = DaemonSettings::parse("[history]\nrange_days = 45\n", None).unwrap_err();
        assert!(error.to_string().contains("range_days"), "{error}");
        assert!(DaemonSettings::parse("[history]\nenabled = \"yes\"\n", None).is_err());
    }

    #[test]
    fn enabling_history_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        set_history_enabled(&path, true).unwrap();
        assert!(DaemonSettings::load(&path, None).unwrap().history.enabled);

        std::fs::write(
            &path,
            "# my settings\n[daemon]\nrestore_sessions = \"stopped\"\n\n[history]\n# off for now\nenabled = false\nrange_days = 90\n",
        )
        .unwrap();
        set_history_enabled(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my settings") && text.contains("# off for now"),
            "{text}"
        );
        let settings = DaemonSettings::load(&path, None).unwrap();
        assert!(settings.history.enabled);
        assert_eq!(settings.history.range_days, 90);
        assert_eq!(settings.restore_sessions, RestoreMode::Stopped);

        set_history_enabled(&path, false).unwrap();
        assert!(!DaemonSettings::load(&path, None).unwrap().history.enabled);
        assert!(!dir.path().join("daemon.toml.tmp").exists());

        std::fs::write(&path, "[history\n").unwrap();
        assert!(set_history_enabled(&path, true).is_err());
    }

    #[test]
    fn a_missing_file_means_defaults_and_a_bad_one_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(
            DaemonSettings::load(&path, None).unwrap(),
            DaemonSettings::default()
        );
        std::fs::write(&path, "[daemon]\nrestore_sessions = 3\n").unwrap();
        let error = DaemonSettings::load(&path, None).unwrap_err();
        assert!(error.to_string().contains("daemon.toml"), "{error}");
    }

    #[test]
    fn the_home_folder_is_absolute_and_expands_a_leading_tilde() {
        let home = Path::new("/Users/u");
        let folder = |text: &str, user_home| {
            DaemonSettings::parse(
                &format!("[workspaces]\nhome_folder = \"{text}\"\n"),
                user_home,
            )
            .map(|s| s.home_folder)
        };
        assert_eq!(
            folder("/srv/notes", None).unwrap(),
            Some("/srv/notes".into())
        );
        assert_eq!(
            folder("~/Notes", Some(home)).unwrap(),
            Some("/Users/u/Notes".into())
        );
        assert_eq!(folder("~", Some(home)).unwrap(), Some("/Users/u".into()));
        assert!(folder("~/Notes", None).is_err());
        assert!(folder("notes", Some(home)).is_err(), "relative");
        assert!(
            folder("~other/x", Some(home)).is_err(),
            "another user's home"
        );
        assert_eq!(DaemonSettings::parse("", None).unwrap().home_folder, None);
    }

    #[test]
    fn assistant_settings_default_to_everything_but_global_files() {
        let settings = DaemonSettings::parse("", None).unwrap().assistant;
        assert_eq!(settings.provider, None);
        assert_eq!(settings.model, None);
        assert_eq!(settings.context_budget_chars, 60_000);
        assert_eq!(settings.context, ContextPermissions::default());
        assert!(settings.context.files && settings.context.shell);
        assert!(!settings.global_context.files);
        assert!(settings.global_context.shell && settings.global_context.agents);
        assert!(settings.global_context.search);
    }

    #[test]
    fn assistant_settings_are_read_and_overridden_per_workspace() {
        let settings = DaemonSettings::parse(
            "[assistant]\nprovider = \"ollama\"\nmodel = \"llama3\"\ncontext_budget_chars = 900\n\
             [assistant.context]\nshell = false\n\
             [assistant.global_context]\nfiles = true\n\
             [assistant.workspaces.secret]\nfiles = false\nsearch = false\n",
            None,
        )
        .unwrap()
        .assistant;
        assert_eq!(settings.provider.as_deref(), Some("ollama"));
        assert_eq!(settings.model.as_deref(), Some("llama3"));
        assert_eq!(settings.context_budget_chars, 900);
        assert!(!settings.context.shell && settings.context.files);
        assert!(settings.global_context.files);
        let secret = settings.permissions_for_workspace("secret", "01WS");
        assert!(!secret.files && !secret.search && !secret.shell && secret.agents);
        // Another workspace has the plain `[assistant.context]`; the id works as a key too.
        assert_eq!(
            settings.permissions_for_workspace("other", "01WS"),
            settings.context
        );
        let by_id = DaemonSettings::parse("[assistant.workspaces.01WS]\nagents = false\n", None)
            .unwrap()
            .assistant;
        assert!(!by_id.permissions_for_workspace("any", "01WS").agents);
    }

    #[test]
    fn assistant_budget_must_be_positive() {
        assert!(DaemonSettings::parse("[assistant]\ncontext_budget_chars = 0\n", None).is_err());
        assert!(DaemonSettings::parse("[assistant.context]\nshell = \"yes\"\n", None).is_err());
    }
}
