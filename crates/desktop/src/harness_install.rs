//! Installing a missing piece of a harness from its original source (design.md § First-run setup,
//! `docs/agents.md`): the Claude Code ACP adapter, OMP, Codex. Only on the user's click, and only
//! after the package has been checked against a table that is fixed here.
//!
//! The daemon never downloads anything and the desktop never runs `npx` or anything that is not
//! installed. What happens instead:
//!
//! 1. [`verify`] asks the registry about the package (`npm view <package>@latest name version
//!    repository.url bin dist.integrity --json`, through [`PackageRunner`]) and accepts it only
//!    when the name, the repository and the executable match the [`Source`] of the harness.
//! 2. The workbench shows the version, the repository and the integrity in a confirmation.
//! 3. On confirmation a terminal tab runs `npm install -g <package>@<version>` (`bun install -g`
//!    for OMP, which is a Bun program), so the user sees the output.
//!
//! Without `npm` (or `bun`) on the user's `PATH` nothing is run: the command and the source
//! link are shown. Tests use [`FakeRunner`] and never reach a registry.

use std::collections::HashMap;
use std::fmt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// The program that installs the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Npm,
    Bun,
}

impl Tool {
    pub fn program(self) -> &'static str {
        match self {
            Tool::Npm => "npm",
            Tool::Bun => "bun",
        }
    }
}

/// Where a harness's package comes from, as Hutan expects to find it.
#[derive(Debug, PartialEq, Eq)]
pub struct Source {
    /// The harness id (`omp`, `claude`, `codex`).
    pub harness: &'static str,
    /// What is installed: the Claude Code ACP adapter, OMP, the Codex CLI.
    pub what: &'static str,
    /// The package on npm.
    pub package: &'static str,
    /// The repository its `package.json` must name, as `host/owner/name` (see
    /// [`normalize_repository`]).
    pub repository: &'static str,
    /// An executable the package must provide.
    pub bin: &'static str,
    pub tool: Tool,
}

impl Source {
    /// The page of the source.
    pub fn link(&self) -> String {
        format!("https://{}", self.repository)
    }

    /// The command a user types to install the latest version by hand.
    pub fn manual_command(&self) -> String {
        format!("{} install -g {}", self.tool.program(), self.package)
    }
}

/// The packages Hutan will offer to install. A package that npm lists under another name,
/// repository or executable is refused, whatever the registry says.
pub const SOURCES: [Source; 3] = [
    Source {
        harness: "claude",
        what: "the Claude Code ACP adapter",
        package: "@agentclientprotocol/claude-agent-acp",
        repository: "github.com/agentclientprotocol/claude-agent-acp",
        bin: "claude-agent-acp",
        tool: Tool::Npm,
    },
    // [INFERENCE] the repository of OMP's package was taken from its name, not checked against
    // the registry: a mismatch is refused, so a wrong entry only costs the install button.
    Source {
        harness: "omp",
        what: "OMP",
        package: "@oh-my-pi/pi-coding-agent",
        repository: "github.com/can1357/oh-my-pi",
        bin: "omp",
        tool: Tool::Bun,
    },
    Source {
        harness: "codex",
        what: "the Codex CLI",
        package: "@openai/codex",
        repository: "github.com/openai/codex",
        bin: "codex",
        tool: Tool::Npm,
    },
];

/// The source of `harness`.
pub fn source(harness: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|s| s.harness == harness)
}

/// The harness a "`claude-agent-acp` was not found" error is about (the daemon names the program
/// it looked for).
pub fn harness_of_missing(message: &str) -> Option<&'static str> {
    if !message.contains("was not found") {
        return None;
    }
    SOURCES
        .iter()
        .find(|s| message.contains(s.bin) || message.contains(s.harness))
        .or_else(|| message.contains("claude-code-acp").then_some(&SOURCES[0]))
        .map(|s| s.harness)
}

/// A package the registry confirmed to be the one in the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub source: &'static Source,
    pub version: String,
    /// `dist.integrity` of that version (a `sha512-…` digest).
    pub integrity: String,
}

impl Verified {
    /// What is installed: `package@version`, exactly the version that was checked.
    pub fn spec(&self) -> String {
        format!("{}@{}", self.source.package, self.version)
    }

    /// The command that runs in the terminal tab.
    pub fn command(&self) -> String {
        format!("{} install -g {}", self.source.tool.program(), self.spec())
    }
}

/// Why nothing is offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// There is no `npm` to ask the registry with (or no `bun` to install OMP with).
    NoTool(&'static str, &'static Source),
    /// The registry could not be asked, or answered nonsense.
    Registry(String),
    /// The registry answers, but not with the package in the table.
    Mismatch(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NoTool(tool, source) => write!(
                f,
                "{tool} was not found, so Hutan cannot check or install {} ({}). Install it \
                 yourself with `{}`; the source is {}.",
                source.what,
                source.package,
                source.manual_command(),
                source.link()
            ),
            Refusal::Registry(message) => write!(f, "Cannot check the package on npm: {message}"),
            Refusal::Mismatch(message) => write!(f, "Not installing: {message}"),
        }
    }
}

/// What Hutan needs from the machine: asking the registry, finding programs, and the command
/// of the terminal tab. Blocking; call off the main thread.
pub trait PackageRunner: Send + Sync + fmt::Debug {
    /// Whether `program` is on the user's `PATH` (the one a terminal of theirs has).
    fn has_program(&self, program: &str) -> bool;
    /// `npm view <spec> name version repository.url bin dist.integrity --json`: the JSON.
    fn view(&self, spec: &str) -> Result<String, String>;
    /// The argv of the terminal tab that installs `spec` with `tool`.
    fn install_argv(&self, tool: Tool, spec: &str) -> Vec<String>;
}

/// `host/owner/name` of a repository URL as `package.json` writes it: `git+https://github.com/o/n.git`,
/// `git://…`, `ssh://git@github.com/o/n`, `git@github.com:o/n.git`, `github:o/n` or `https://…`.
pub fn normalize_repository(url: &str) -> String {
    let mut text = url.trim().to_lowercase();
    if let Some(shorthand) = text.strip_prefix("github:") {
        text = format!("github.com/{shorthand}");
    }
    for prefix in ["git+", "https://", "http://", "ssh://", "git://", "git@"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_owned();
        }
    }
    if let Some(rest) = text.strip_prefix("git@") {
        text = rest.to_owned();
    }
    let text = text.replacen(':', "/", 1);
    let text = text.trim_end_matches('/');
    text.strip_suffix(".git").unwrap_or(text).to_owned()
}

/// Asks the registry about `source` and accepts the answer only when it is the table's package.
pub fn verify(runner: &dyn PackageRunner, source: &'static Source) -> Result<Verified, Refusal> {
    if !runner.has_program("npm") {
        return Err(Refusal::NoTool("npm", source));
    }
    if source.tool == Tool::Bun && !runner.has_program("bun") {
        return Err(Refusal::NoTool("bun", source));
    }
    let text = runner
        .view(&format!("{}@latest", source.package))
        .map_err(Refusal::Registry)?;
    let json: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|error| Refusal::Registry(format!("unreadable answer: {error}")))?;
    let field = |name: &str| -> Option<&serde_json::Value> {
        // `npm view` prints the dotted names flat; accept the nested form too.
        json.get(name).or_else(|| {
            name.split('.')
                .try_fold(&json, |value, part| value.get(part))
        })
    };
    let text_of = |name: &str| field(name).and_then(serde_json::Value::as_str);
    let name = text_of("name").unwrap_or_default();
    if name != source.package {
        return Err(Refusal::Mismatch(format!(
            "npm answered with the package “{name}”, not {}.",
            source.package
        )));
    }
    let repository = text_of("repository.url").unwrap_or_default();
    if normalize_repository(repository) != source.repository {
        return Err(Refusal::Mismatch(format!(
            "{} names the repository “{repository}”, not {}.",
            source.package, source.repository
        )));
    }
    let has_bin = match field("bin") {
        Some(serde_json::Value::Object(bins)) => bins.contains_key(source.bin),
        // A bare string is the executable named like the package, without its scope.
        Some(serde_json::Value::String(_)) => source.package.rsplit('/').next() == Some(source.bin),
        _ => false,
    };
    if !has_bin {
        return Err(Refusal::Mismatch(format!(
            "{} does not provide the executable {}.",
            source.package, source.bin
        )));
    }
    let version = text_of("version").unwrap_or_default();
    if version.is_empty() || version.contains(char::is_whitespace) {
        return Err(Refusal::Registry("it named no version".to_owned()));
    }
    let integrity = text_of("dist.integrity").unwrap_or_default();
    if integrity.is_empty() {
        return Err(Refusal::Registry(format!(
            "it gave no integrity for {version}"
        )));
    }
    Ok(Verified {
        source,
        version: version.to_owned(),
        integrity: integrity.to_owned(),
    })
}

/// The real thing: the user's login shell finds the programs (an app started from the Dock has
/// a bare `PATH`), and the terminal tab runs the install through `sh`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl SystemRunner {
    fn login_shell() -> String {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/zsh".to_owned())
    }

    /// Runs `script "$@"` with `args` in a login shell.
    fn shell(script: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new(Self::login_shell())
            .args(["-l", "-c", script, "sh"])
            .args(args)
            .stdin(Stdio::null())
            .output()
    }
}

impl PackageRunner for SystemRunner {
    fn has_program(&self, program: &str) -> bool {
        Self::shell(r#"command -v "$1" >/dev/null 2>&1"#, &[program])
            .is_ok_and(|output| output.status.success())
    }

    fn view(&self, spec: &str) -> Result<String, String> {
        let output = Self::shell(
            r#"exec npm view "$@""#,
            &[
                spec,
                "name",
                "version",
                "repository.url",
                "bin",
                "dist.integrity",
                "--json",
            ],
        )
        .map_err(|error| format!("cannot run npm: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
            Err(last.unwrap_or("npm view failed").trim().to_owned())
        }
    }

    fn install_argv(&self, tool: Tool, spec: &str) -> Vec<String> {
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("{} install -g '{}'", tool.program(), spec.replace('\'', "")),
        ]
    }
}

#[derive(Debug, Default)]
struct FakeState {
    programs: Vec<String>,
    answers: HashMap<String, Result<String, String>>,
    views: Vec<String>,
    exit_code: i32,
}

/// A scripted registry for tests: records the questions, answers as told. The install tab runs
/// `exit <code>` instead of an install. Cloning shares the state.
#[derive(Debug, Clone)]
pub struct FakeRunner {
    state: Arc<Mutex<FakeState>>,
}

impl FakeRunner {
    /// `npm` and `bun` are on the `PATH`; nothing is known about any package.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                programs: vec!["npm".to_owned(), "bun".to_owned()],
                ..FakeState::default()
            })),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        match self.state.lock() {
            Ok(mut state) => f(&mut state),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    /// Which programs exist.
    pub fn set_programs(&self, programs: &[&str]) {
        self.with(|state| state.programs = programs.iter().map(|p| (*p).to_owned()).collect());
    }

    /// What `npm view` prints for `package@latest`.
    pub fn answer(&self, package: &str, answer: Result<&str, &str>) {
        self.with(|state| {
            state.answers.insert(
                format!("{package}@latest"),
                answer.map(str::to_owned).map_err(str::to_owned),
            );
        });
    }

    /// The `npm view` answer of a package that is what the table says it is.
    pub fn genuine(&self, source: &Source, version: &str) {
        let bin = serde_json::json!({ source.bin: "dist/index.js" });
        let answer = serde_json::json!({
            "name": source.package,
            "version": version,
            "repository.url": format!("git+https://{}.git", source.repository),
            "bin": bin,
            "dist.integrity": "sha512-GENUINE",
        });
        self.answer(source.package, Ok(&answer.to_string()));
    }

    /// The exit code of the install tab.
    pub fn set_install_exit(&self, code: i32) {
        self.with(|state| state.exit_code = code);
    }

    /// The specs `npm view` was asked about.
    pub fn views(&self) -> Vec<String> {
        self.with(|state| state.views.clone())
    }
}

impl Default for FakeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRunner for FakeRunner {
    fn has_program(&self, program: &str) -> bool {
        self.with(|state| state.programs.iter().any(|p| p == program))
    }

    fn view(&self, spec: &str) -> Result<String, String> {
        self.with(|state| {
            state.views.push(spec.to_owned());
            state
                .answers
                .get(spec)
                .cloned()
                .unwrap_or_else(|| Err("404 not found".to_owned()))
        })
    }

    fn install_argv(&self, _tool: Tool, _spec: &str) -> Vec<String> {
        let code = self.with(|state| state.exit_code);
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("exit {code}"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> &'static Source {
        source("claude").unwrap()
    }

    #[test]
    fn repositories_compare_whatever_their_spelling() {
        let expected = "github.com/agentclientprotocol/claude-agent-acp";
        for url in [
            "git+https://github.com/agentclientprotocol/claude-agent-acp.git",
            "https://github.com/agentclientprotocol/claude-agent-acp",
            "git://github.com/AgentClientProtocol/claude-agent-acp.git",
            "git@github.com:agentclientprotocol/claude-agent-acp.git",
            "ssh://git@github.com/agentclientprotocol/claude-agent-acp.git",
            "github:agentclientprotocol/claude-agent-acp",
            "https://github.com/agentclientprotocol/claude-agent-acp/",
        ] {
            assert_eq!(normalize_repository(url), expected, "{url}");
        }
        assert_ne!(
            normalize_repository("https://github.com/evil/claude-agent-acp"),
            expected
        );
    }

    #[test]
    fn a_genuine_package_is_verified_with_its_version_and_integrity() {
        let runner = FakeRunner::new();
        runner.genuine(claude(), "0.93.1");
        let verified = verify(&runner, claude()).unwrap();
        assert_eq!(verified.version, "0.93.1");
        assert_eq!(verified.integrity, "sha512-GENUINE");
        assert_eq!(
            verified.command(),
            "npm install -g @agentclientprotocol/claude-agent-acp@0.93.1"
        );
        assert_eq!(
            runner.views(),
            ["@agentclientprotocol/claude-agent-acp@latest"]
        );
    }

    #[test]
    fn a_package_that_is_not_the_one_in_the_table_is_refused() {
        let runner = FakeRunner::new();
        let source = claude();
        let answer = |name: &str, repo: &str, bin: serde_json::Value| {
            serde_json::json!({
                "name": name, "version": "1.0.0", "repository.url": repo,
                "bin": bin, "dist.integrity": "sha512-X",
            })
            .to_string()
        };
        let bins = serde_json::json!({ "claude-agent-acp": "x.js" });
        for (answer, what) in [
            (
                answer(
                    source.package,
                    "git+https://github.com/someone-else/claude-agent-acp.git",
                    bins.clone(),
                ),
                "repository",
            ),
            (
                answer(
                    "@evil/claude-agent-acp",
                    "github.com/agentclientprotocol/claude-agent-acp",
                    bins.clone(),
                ),
                "package",
            ),
            (
                answer(
                    source.package,
                    "https://github.com/agentclientprotocol/claude-agent-acp",
                    serde_json::json!({ "something-else": "x.js" }),
                ),
                "executable",
            ),
        ] {
            runner.answer(source.package, Ok(&answer));
            let refusal = verify(&runner, source).unwrap_err();
            assert!(
                matches!(refusal, Refusal::Mismatch(_)),
                "{what}: {refusal:?}"
            );
            assert!(
                refusal.to_string().starts_with("Not installing"),
                "{refusal}"
            );
        }
        // The registry in trouble is not a mismatch.
        runner.answer(source.package, Err("ENOTFOUND"));
        assert_eq!(
            verify(&runner, source),
            Err(Refusal::Registry("ENOTFOUND".to_owned()))
        );
        runner.answer(source.package, Ok("not json"));
        assert!(matches!(verify(&runner, source), Err(Refusal::Registry(_))));
    }

    #[test]
    fn without_npm_or_bun_the_command_and_the_source_are_shown() {
        let runner = FakeRunner::new();
        runner.set_programs(&[]);
        let refusal = verify(&runner, claude()).unwrap_err();
        let text = refusal.to_string();
        assert!(text.contains("npm install -g @agentclientprotocol/claude-agent-acp"));
        assert!(text.contains("https://github.com/agentclientprotocol/claude-agent-acp"));
        assert!(
            runner.views().is_empty(),
            "nothing was asked of the registry"
        );

        // OMP is installed with Bun, which has to exist as well.
        runner.set_programs(&["npm"]);
        let omp = source("omp").unwrap();
        let refusal = verify(&runner, omp).unwrap_err();
        assert_eq!(refusal, Refusal::NoTool("bun", omp));
        assert!(
            refusal
                .to_string()
                .contains("bun install -g @oh-my-pi/pi-coding-agent")
        );
    }

    #[test]
    fn a_not_found_error_names_the_harness() {
        assert_eq!(
            harness_of_missing("`claude-agent-acp` was not found on PATH; install it"),
            Some("claude")
        );
        assert_eq!(
            harness_of_missing("`claude-code-acp` was not found on PATH"),
            Some("claude")
        );
        assert_eq!(harness_of_missing("`omp` was not found"), Some("omp"));
        assert_eq!(harness_of_missing("`codex` was not found"), Some("codex"));
        assert_eq!(harness_of_missing("the model failed"), None);
        assert_eq!(harness_of_missing("`gemini` was not found"), None);
    }

    #[test]
    fn the_table_matches_the_documented_packages() {
        let packages: Vec<_> = SOURCES
            .iter()
            .map(|s| (s.harness, s.package, s.tool))
            .collect();
        assert_eq!(
            packages,
            [
                ("claude", "@agentclientprotocol/claude-agent-acp", Tool::Npm),
                ("omp", "@oh-my-pi/pi-coding-agent", Tool::Bun),
                ("codex", "@openai/codex", Tool::Npm),
            ]
        );
    }
}
