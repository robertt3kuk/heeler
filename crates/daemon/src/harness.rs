//! The harness registry (ADR 0015): which agent harnesses chats can run through over ACP, how
//! each is started, and where its own CLI is.
//!
//! - `omp`: `omp acp`, first-class.
//! - `claude`: Claude Code through its ACP adapter. Hutan points to an installed one and does
//!   not bundle or download it: the official `claude-agent-acp`
//!   (`@agentclientprotocol/claude-agent-acp`) first, else the older `claude-code-acp`
//!   (`@zed-industries/claude-code-acp`).
//!
//! Both are `[harness.<id>]` of `daemon.toml` (`bin`, `args`, `env`, `cli`) and are found on the
//! `PATH` of the user's login environment otherwise: a GUI-launched daemon has a shorter `PATH`
//! than a shell, and `omp` lives in `~/.bun/bin`. Their processes start from that environment
//! (as terminal sessions with an explicit command do), so they find their own configuration and
//! logins. Hutan reads none of it.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use hutan_agents::AcpConfig;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_terminal::EnvInherit;
use serde_json::{Value, json};

use crate::config::{HarnessSettings, McpSettings};

/// OMP, over `omp acp`.
pub const AGENT_OMP: &str = "omp";
/// Claude Code, over its ACP adapter.
pub const AGENT_CLAUDE: &str = "claude";

/// Whether sessions of this agent are ACP harness chats.
pub fn is_harness_agent(agent: &str) -> bool {
    agent == AGENT_OMP || agent == AGENT_CLAUDE
}

/// The tools an OMP assistant keeps (`omp acp --tools`): read-only file and search tools, by OMP's
/// own names (an unknown name fails `session/new` with "Built-in tool unavailable"). No
/// `bash`, `edit`, `write`, `task` or network tools.
pub const OMP_ASSISTANT_TOOLS: &str = "read,grep,glob";

/// The session mode a Claude Code assistant runs in: its read-only planning mode.
pub const CLAUDE_ASSISTANT_MODE: &str = "plan";

/// Appended to the system prompt of an OMP assistant (`--append-system-prompt`).
pub const HARNESS_ASSISTANT_PROMPT: &str = "You are Hutan's assistant, built into a developer's \
terminal workspace. Answer concisely and concretely. A message may carry attached context: \
read-only excerpts the user chose to share (terminal output, files, conversations with \
coding agents, search results), each introduced by a bracketed header that says what it is \
and whether it was cut. Rely on that context and on the conversation; if the answer needs \
something that was not attached, say what to attach instead of guessing. You can read files \
and search the workspace, but you cannot run commands or change anything.";

/// What is fixed about a harness.
struct Def {
    /// The ACP executables looked for on `PATH`, preferred first.
    acp_bins: &'static [&'static str],
    acp_args: &'static [&'static str],
    /// The harness's own CLI (accounts).
    cli: &'static str,
    /// What to tell a user whose harness is missing.
    install: &'static str,
}

fn def(agent: &str) -> Option<Def> {
    match agent {
        // Codex is not an ACP harness, but its CLI is found the same way (accounts).
        "codex" => Some(Def {
            acp_bins: &["codex"],
            acp_args: &["app-server"],
            cli: "codex",
            install: "install Codex (`npm install -g @openai/codex`)",
        }),
        AGENT_OMP => Some(Def {
            acp_bins: &["omp"],
            acp_args: &["acp"],
            cli: "omp",
            install: "install OMP (`bun install -g @oh-my-pi/pi-coding-agent`)",
        }),
        AGENT_CLAUDE => Some(Def {
            acp_bins: &["claude-agent-acp", "claude-code-acp"],
            acp_args: &[],
            cli: "claude",
            install: "install the Claude Code ACP adapter (`npm install -g @agentclientprotocol/claude-agent-acp`)",
        }),
        _ => None,
    }
}

/// The harnesses of one daemon.
#[derive(Debug)]
pub struct Harnesses {
    settings: BTreeMap<String, HarnessSettings>,
    mcp: Vec<Value>,
    env_inherit: EnvInherit,
    login_shell: Option<String>,
}

impl Harnesses {
    pub fn new(
        settings: BTreeMap<String, HarnessSettings>,
        mcp: &BTreeMap<String, McpSettings>,
        env_inherit: EnvInherit,
        login_shell: Option<String>,
    ) -> Self {
        Self {
            settings,
            mcp: mcp
                .iter()
                .map(|(name, server)| mcp_server(name, server))
                .collect(),
            env_inherit,
            login_shell,
        }
    }

    /// The MCP servers handed to every harness session, as ACP `McpServer` objects.
    pub fn mcp_servers(&self) -> &[Value] {
        &self.mcp
    }

    /// The environment harness processes start from: the user's login environment (or, with
    /// `[terminal] env_inherit = "daemon"`, the daemon's own: nothing is added). Blocks: the
    /// first capture of a login shell's environment is a subprocess.
    pub fn environment(&self) -> BTreeMap<String, String> {
        if self.env_inherit == EnvInherit::Daemon {
            return BTreeMap::new();
        }
        let facts = hutan_terminal::env::system_facts();
        let mut env = hutan_terminal::env::base_env(&facts);
        let shell = self.login_shell.clone().or_else(|| facts.shell.clone());
        if let Some(shell) = shell {
            env.insert("SHELL".to_owned(), shell.clone());
            if let Some(login) = hutan_terminal::env::login_env(&shell, &facts) {
                env.extend(login);
            }
        }
        env
    }

    fn settings_of(&self, agent: &str) -> HarnessSettings {
        self.settings.get(agent).cloned().unwrap_or_default()
    }

    /// `acp_config`'s program and the environment it will run in: `configured`, else the first
    /// of `candidates` found on `PATH`, else in the directories installers use (see
    /// [`install_dirs`]): a daemon started by the app has a short `PATH`, and some shells only
    /// extend it for interactive sessions, so `omp` in `~/.bun/bin` or `claude` in
    /// `~/.local/bin` must still be found however they were installed.
    fn locate(
        &self,
        agent: &str,
        configured: Option<&Path>,
        candidates: &[&str],
        env: &BTreeMap<String, String>,
    ) -> Result<PathBuf, ErrorBody> {
        // The login environment's `PATH` first, then the daemon's own (inherited from whoever
        // started it: a terminal's full `PATH` when `hutan` autostarted it from a shell).
        let own = std::env::var("PATH").ok();
        let path = match (env.get("PATH"), own) {
            (Some(login), Some(own)) if *login != own => Some(format!("{login}:{own}")),
            (Some(login), _) => Some(login.clone()),
            (None, own) => own,
        };
        let home = env
            .get("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
        let fallback = home.as_deref().map(install_dirs).unwrap_or_default();
        let names: Vec<String> = match configured {
            Some(p) => vec![p.to_string_lossy().into_owned()],
            None => candidates.iter().map(|c| (*c).to_owned()).collect(),
        };
        names
            .iter()
            .find_map(|name| {
                find_program(name, path.as_deref()).or_else(|| find_in(name, &fallback))
            })
            .ok_or_else(|| {
                let install = def(agent).map_or("", |d| d.install);
                let table = if agent == "codex" {
                    "[codex]".to_owned()
                } else {
                    format!("[harness.{agent}]")
                };
                let tried = names
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                ErrorBody::new(
                    ErrorCode::ProviderError,
                    format!(
                        "{tried} was not found{}; {install}, or set `bin` under `{table}` in daemon.toml",
                        if configured.is_some() {
                            ""
                        } else {
                            " on PATH or in the usual install folders"
                        },
                    ),
                )
            })
    }

    /// How to start `agent`'s ACP process. Blocks (see [`environment`](Self::environment)).
    ///
    /// With `assistant` the process starts with its tools restricted to reading (protocol 0.20,
    /// `docs/assistant.md` § Backends): OMP gets `--tools` and the assistant prompt appended to
    /// its system prompt. Claude Code is restricted by its session mode (`plan`), which the
    /// assistant service requests when it creates the session.
    pub fn acp_config(&self, agent: &str, assistant: bool) -> Result<AcpConfig, ErrorBody> {
        let def = def(agent)
            .filter(|_| is_harness_agent(agent))
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::BadRequest,
                    format!("`{agent}` is not an ACP harness (omp, claude)"),
                )
            })?;
        let settings = self.settings_of(agent);
        let mut env = self.environment();
        let program = self.locate(agent, settings.bin.as_deref(), def.acp_bins, &env)?;
        let mut args: Vec<String> = if settings.args.is_empty() {
            def.acp_args.iter().map(|a| (*a).to_owned()).collect()
        } else {
            settings.args.clone()
        };
        if assistant && agent == AGENT_OMP {
            args.extend([
                "--tools".to_owned(),
                OMP_ASSISTANT_TOOLS.to_owned(),
                "--append-system-prompt".to_owned(),
                HARNESS_ASSISTANT_PROMPT.to_owned(),
            ]);
        }
        env.extend(settings.env);
        with_program_dir(&mut env, &program);
        Ok(AcpConfig {
            agent: agent.to_owned(),
            program,
            args,
            env: env.into_iter().collect(),
        })
    }

    /// The harness's own CLI, and the environment to run it in (accounts: status, login,
    /// logout). For `codex` the CLI is `codex_bin` (`[codex] bin`). Blocks.
    pub fn cli(
        &self,
        agent: &str,
        codex_bin: &Path,
    ) -> Result<(PathBuf, BTreeMap<String, String>), ErrorBody> {
        let Some(def) = def(agent) else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("`{agent}` is not a harness with accounts (omp, claude, codex)"),
            ));
        };
        if agent == "codex" {
            let mut env = self.environment();
            let program = self.locate(agent, Some(codex_bin), &[def.cli], &env)?;
            with_program_dir(&mut env, &program);
            return Ok((program, env));
        }
        let settings = self.settings_of(agent);
        let mut env = self.environment();
        // OMP's ACP executable is its CLI.
        let configured = settings.cli.as_deref().or_else(|| {
            (agent == AGENT_OMP)
                .then_some(settings.bin.as_deref())
                .flatten()
        });
        let program = self.locate(agent, configured, &[def.cli], &env)?;
        with_program_dir(&mut env, &program);
        Ok((program, env))
    }
}

/// An ACP `McpServer` for a `[mcp.<name>]` table (already validated).
fn mcp_server(name: &str, server: &McpSettings) -> Value {
    match (&server.command, &server.url) {
        (Some(command), _) => json!({
            "name": name,
            "command": command,
            "args": server.args,
            "env": [],
        }),
        (None, Some(url)) => json!({
            "type": server.transport.as_deref().unwrap_or("http"),
            "name": name,
            "url": url,
            "headers": [],
        }),
        (None, None) => json!({"name": name}),
    }
}

/// `name` as an executable: a path as it is, a bare name searched along `path`.
pub fn find_program(name: &str, path: Option<&str>) -> Option<PathBuf> {
    let executable = |candidate: &Path| {
        candidate
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        let candidate = PathBuf::from(name);
        return executable(&candidate).then_some(candidate);
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

/// Directories installers put command-line tools in, below `home` and system-wide, newest
/// version-managed Node first: Bun, `~/.local/bin` (Claude Code, Codex, pipx), Cargo, npm with a
/// user prefix, pnpm, Yarn, Volta, mise/asdf shims, nvm/fnm Node versions, Nix, Homebrew and
/// `/usr/local/bin`.
pub fn install_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        ".bun/bin",
        ".local/bin",
        ".cargo/bin",
        ".npm-global/bin",
        ".npm/bin",
        "Library/pnpm",
        ".local/share/pnpm",
        ".yarn/bin",
        ".config/yarn/global/node_modules/.bin",
        ".volta/bin",
        ".local/share/mise/shims",
        ".asdf/shims",
        ".nix-profile/bin",
        "bin",
    ]
    .iter()
    .map(|sub| home.join(sub))
    .collect();
    for versions in [
        home.join(".nvm/versions/node"),
        home.join("Library/Application Support/fnm/node-versions"),
        home.join(".local/share/fnm/node-versions"),
        home.join(".fnm/node-versions"),
    ] {
        let Ok(entries) = std::fs::read_dir(&versions) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        // Newest first: `v22.1.0` sorts after `v20.9.0` by version, not by name.
        found.sort_by_key(|p| std::cmp::Reverse(node_version(p)));
        for version in found {
            for bin in [version.join("bin"), version.join("installation/bin")] {
                if bin.is_dir() {
                    dirs.push(bin);
                }
            }
        }
    }
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/nix/var/nix/profiles/default/bin",
        ]
        .iter()
        .map(PathBuf::from),
    );
    dirs
}

/// `v20.11.1` → `(20, 11, 1)`; anything else sorts first.
fn node_version(path: &Path) -> (u32, u32, u32) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parts = name
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// A bare `name` in one of `dirs`.
fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if name.contains('/') {
        return None;
    }
    let path = std::env::join_paths(dirs).ok()?;
    find_program(name, path.to_str())
}

/// Puts `program`'s folder at the front of `env`'s `PATH` (the daemon's own when `env` has
/// none) unless it is there: a tool found outside `PATH` is often a script whose interpreter
/// (`#!/usr/bin/env node`, `bun`) lives next to it.
fn with_program_dir(env: &mut BTreeMap<String, String>, program: &Path) {
    let Some(dir) = program.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let current = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    if std::env::split_paths(&current).any(|p| p == dir) {
        return;
    }
    let joined = std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&current)),
    );
    if let Ok(joined) = joined {
        env.insert("PATH".to_owned(), joined.to_string_lossy().into_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn programs_are_found_on_the_given_path_only_when_executable() {
        let dir = tempfile::tempdir().unwrap();
        let tool = executable(dir.path(), "claude-code-acp");
        std::fs::write(dir.path().join("plain"), "").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            find_program("claude-code-acp", Some(&path)),
            Some(tool.clone())
        );
        assert_eq!(find_program("plain", Some(&path)), None, "not executable");
        assert_eq!(find_program("absent", Some(&path)), None);
        assert_eq!(find_program(&tool.to_string_lossy(), None), Some(tool));
        assert_eq!(find_program("claude-code-acp", None), None);
    }

    #[test]
    fn a_tool_missing_from_path_is_found_in_the_install_folders_and_its_folder_joins_path() {
        let home = tempfile::tempdir().unwrap();
        let bun = home.path().join(".bun/bin");
        std::fs::create_dir_all(&bun).unwrap();
        let tool = executable(&bun, "hutan-discovery-test-tool");
        let h = harnesses(&[]);
        let mut env = BTreeMap::new();
        env.insert(
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        );
        env.insert("PATH".to_owned(), "/nonexistent-hutan-test".to_owned());
        let found = h
            .locate("omp", None, &["hutan-discovery-test-tool"], &env)
            .unwrap();
        assert_eq!(found, tool);

        with_program_dir(&mut env, &found);
        assert!(env["PATH"].starts_with(&bun.to_string_lossy().into_owned()));
        let before = env["PATH"].clone();
        with_program_dir(&mut env, &found);
        assert_eq!(env["PATH"], before, "added once");

        let missing = h
            .locate("omp", None, &["hutan-discovery-absent-tool"], &env)
            .unwrap_err();
        assert!(
            missing.message.contains("usual install folders"),
            "{}",
            missing.message
        );
    }

    #[test]
    fn the_newest_version_managed_node_comes_first() {
        let home = tempfile::tempdir().unwrap();
        for v in ["v9.11.2", "v20.9.0", "v22.1.0"] {
            std::fs::create_dir_all(home.path().join(".nvm/versions/node").join(v).join("bin"))
                .unwrap();
        }
        let dirs = install_dirs(home.path());
        let nvm: Vec<String> = dirs
            .iter()
            .filter(|d| d.starts_with(home.path().join(".nvm")))
            .map(|d| {
                d.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(nvm, ["v22.1.0", "v20.9.0", "v9.11.2"]);
    }

    fn harnesses(settings: &[(&str, HarnessSettings)]) -> Harnesses {
        Harnesses::new(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            &BTreeMap::new(),
            EnvInherit::Daemon,
            None,
        )
    }

    #[test]
    fn a_missing_harness_says_what_to_install_and_which_key_to_set() {
        let dir = tempfile::tempdir().unwrap();
        // The daemon's PATH is used in `daemon` mode; give the lookup one without the tool.
        let h = harnesses(&[(
            "claude",
            HarnessSettings {
                bin: Some(dir.path().join("nope")),
                ..HarnessSettings::default()
            },
        )]);
        let error = h.acp_config("claude", false).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProviderError);
        assert!(error.message.contains("nope"), "{}", error.message);
        assert!(
            error.message.contains("[harness.claude]"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("@agentclientprotocol/claude-agent-acp"),
            "{}",
            error.message
        );
    }

    #[test]
    fn the_official_claude_adapter_wins_over_the_older_one() {
        let old = tempfile::tempdir().unwrap();
        executable(old.path(), "claude-code-acp");
        let new = tempfile::tempdir().unwrap();
        let official = executable(new.path(), "claude-agent-acp");
        let h = harnesses(&[]);
        let mut env = BTreeMap::new();
        // The older adapter comes first on PATH; the official one is still preferred.
        let path = std::env::join_paths([old.path(), new.path()]).unwrap();
        env.insert("PATH".to_owned(), path.to_string_lossy().into_owned());
        let found = h
            .locate(
                "claude",
                None,
                &["claude-agent-acp", "claude-code-acp"],
                &env,
            )
            .unwrap();
        assert_eq!(found, official);
        env.insert("PATH".to_owned(), old.path().to_string_lossy().into_owned());
        let found = h
            .locate(
                "claude",
                None,
                &["claude-agent-acp", "claude-code-acp"],
                &env,
            )
            .unwrap();
        assert_eq!(
            found,
            old.path().join("claude-code-acp"),
            "the older one still works"
        );
    }

    #[test]
    fn configured_programs_args_and_env_win_over_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let bin = executable(dir.path(), "my-omp");
        let h = harnesses(&[(
            "omp",
            HarnessSettings {
                bin: Some(bin.clone()),
                args: vec!["acp".into(), "--profile".into(), "work".into()],
                env: BTreeMap::from([("OMP_PROFILE".to_owned(), "work".to_owned())]),
                cli: None,
            },
        )]);
        let config = h.acp_config("omp", false).unwrap();
        assert_eq!(config.program, bin);
        assert_eq!(config.args, ["acp", "--profile", "work"]);
        assert!(
            config
                .env
                .contains(&("OMP_PROFILE".to_owned(), "work".to_owned()))
        );
        assert_eq!(config.agent, "omp");
        // OMP's own CLI is its ACP executable unless told otherwise.
        assert_eq!(h.cli("omp", Path::new("codex")).unwrap().0, bin);
    }

    #[test]
    fn an_omp_assistant_keeps_only_read_tools_and_gets_its_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let bin = executable(dir.path(), "my-omp");
        let h = harnesses(&[(
            "omp",
            HarnessSettings {
                bin: Some(bin),
                args: vec!["acp".into(), "--profile".into(), "work".into()],
                ..HarnessSettings::default()
            },
        )]);
        let plain = h.acp_config("omp", false).unwrap();
        assert!(!plain.args.iter().any(|a| a == "--tools"));
        let assistant = h.acp_config("omp", true).unwrap();
        assert_eq!(
            assistant.args[..3],
            ["acp", "--profile", "work"],
            "the configured arguments stay"
        );
        let tools = assistant.args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(assistant.args[tools + 1], OMP_ASSISTANT_TOOLS);
        for forbidden in ["bash", "edit", "write", "task"] {
            assert!(
                !OMP_ASSISTANT_TOOLS.split(',').any(|t| t == forbidden),
                "{forbidden} is not a read tool"
            );
        }
        let prompt = assistant
            .args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert_eq!(assistant.args[prompt + 1], HARNESS_ASSISTANT_PROMPT);
    }

    #[test]
    fn mcp_servers_become_acp_objects() {
        let servers = BTreeMap::from([
            (
                "docs".to_owned(),
                McpSettings {
                    command: Some("npx".into()),
                    args: vec!["-y".into(), "x".into()],
                    ..McpSettings::default()
                },
            ),
            (
                "search".to_owned(),
                McpSettings {
                    url: Some("https://mcp.example.com/mcp".into()),
                    transport: Some("sse".into()),
                    ..McpSettings::default()
                },
            ),
        ]);
        let h = Harnesses::new(BTreeMap::new(), &servers, EnvInherit::Daemon, None);
        assert_eq!(
            h.mcp_servers(),
            [
                json!({"name": "docs", "command": "npx", "args": ["-y", "x"], "env": []}),
                json!({"type": "sse", "name": "search", "url": "https://mcp.example.com/mcp", "headers": []}),
            ]
        );
    }

    #[test]
    fn only_omp_and_claude_are_harnesses() {
        assert!(is_harness_agent("omp") && is_harness_agent("claude"));
        assert!(!is_harness_agent("codex") && !is_harness_agent("direct"));
        let h = harnesses(&[]);
        assert_eq!(
            h.acp_config("codex", false).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }
}
