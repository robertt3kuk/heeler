//! `heeler integration install | uninstall | status <codex|claude|omp>`: hooks and extensions
//! that make the coding agents report to the session they run in (`docs/integrations.md`).
//!
//! An install touches only what is Heeler's: its own script or extension file, and (Codex,
//! Claude Code) its own marked hook entries inside the agent's configuration. That file is
//! copied to `<name>.heeler-<unix ms>.bak` before it changes, entries of other tools stay
//! as they are, and an uninstall removes exactly what the install added.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::args::IntegrationTarget;
use crate::error::CliError;

mod hooks_json;

/// The script both hook-based integrations run (`integrations/hooks/`).
const HOOK_SCRIPT: &str = include_str!("../../../../integrations/hooks/heeler-agent-state.sh");
/// The OMP extension (`integrations/omp/`).
const OMP_EXTENSION: &str = include_str!("../../../../integrations/omp/heeler-agent-state.ts");
/// Present in everything Heeler writes as a whole file; files without it are never overwritten.
const FILE_MARKER: &str = "heeler-managed";

const SCRIPT_NAME: &str = "heeler-agent-state.sh";
const EXTENSION_NAME: &str = "heeler-agent-state.ts";

/// Events Codex reports through (its permission prompts are `PermissionRequest`).
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
];
/// Events Claude Code reports through; subagents are counted, `SessionEnd` releases.
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Installed,
    /// Some hooks or the script are missing, or the script is outdated.
    Partial,
    NotInstalled,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub target: &'static str,
    /// The agent's configuration directory that was used.
    pub dir: PathBuf,
    pub status: Status,
    /// The operation changed something on disk.
    pub changed: bool,
    /// Files created, rewritten, or removed.
    pub files: Vec<PathBuf>,
    /// Copies of the agent's configuration made before editing it.
    pub backups: Vec<PathBuf>,
    /// What is missing when `status` is `partial`.
    pub missing: Vec<String>,
}

impl Report {
    fn new(target: IntegrationTarget, dir: &Path) -> Self {
        Self {
            target: name(target),
            dir: dir.to_owned(),
            status: Status::NotInstalled,
            changed: false,
            files: Vec::new(),
            backups: Vec::new(),
            missing: Vec::new(),
        }
    }
}

pub fn name(target: IntegrationTarget) -> &'static str {
    match target {
        IntegrationTarget::Codex => "codex",
        IntegrationTarget::Claude => "claude",
        IntegrationTarget::Omp => "omp",
    }
}

/// The agent's configuration directory: `--dir`, else the agent's own environment variable,
/// else its default under `$HOME`.
pub fn config_dir(target: IntegrationTarget, explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(dir) = explicit {
        return Ok(std::path::absolute(dir)?);
    }
    let (variable, default): (&str, &[&str]) = match target {
        IntegrationTarget::Codex => ("CODEX_HOME", &[".codex"]),
        IntegrationTarget::Claude => ("CLAUDE_CONFIG_DIR", &[".claude"]),
        IntegrationTarget::Omp => ("PI_CODING_AGENT_DIR", &[".omp", "agent"]),
    };
    if let Some(dir) = std::env::var_os(variable).filter(|dir| !dir.is_empty()) {
        return Ok(std::path::absolute(PathBuf::from(dir))?);
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| {
            CliError::Usage(format!(
                "cannot tell where {} keeps its configuration: set {variable} or pass --dir",
                name(target)
            ))
        })?;
    Ok(default
        .iter()
        .fold(PathBuf::from(home), |dir, part| dir.join(part)))
}

pub fn install(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    let mut report = Report::new(target, dir);
    match &layout.config {
        Some(config) => {
            // Nothing is written before the configuration is known to be editable, and the
            // script goes in before any hook points at it.
            let command = hook_command(target, &layout.script);
            let events = layout.events;
            let edit = plan_config(config, |doc| hooks_json::install(doc, events, &command))?;
            write_managed(&layout.script, layout.script_text, 0o755, &mut report)?;
            edit.apply(&mut report)?;
        }
        None => write_managed(&layout.script, layout.script_text, 0o644, &mut report)?,
    }
    finish_status(&layout, &mut report)?;
    Ok(report)
}

pub fn uninstall(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    let mut report = Report::new(target, dir);
    if let Some(config) = &layout.config {
        plan_config(config, hooks_json::uninstall)?.apply(&mut report)?;
    }
    remove_managed(&layout.script, &mut report)?;
    finish_status(&layout, &mut report)?;
    Ok(report)
}

pub fn status(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    let mut report = Report::new(target, dir);
    finish_status(&layout, &mut report)?;
    Ok(report)
}

/// Where an integration puts things.
struct Layout {
    /// The agent's hooks file (Codex, Claude Code); `None` for OMP, which only loads a file.
    config: Option<PathBuf>,
    /// The script or extension Heeler owns.
    script: PathBuf,
    script_text: &'static str,
    events: &'static [&'static str],
}

impl Layout {
    fn of(target: IntegrationTarget, dir: &Path) -> Self {
        match target {
            IntegrationTarget::Codex => Self {
                config: Some(dir.join("hooks.json")),
                script: dir.join(SCRIPT_NAME),
                script_text: HOOK_SCRIPT,
                events: CODEX_EVENTS,
            },
            IntegrationTarget::Claude => Self {
                config: Some(dir.join("settings.json")),
                script: dir.join("hooks").join(SCRIPT_NAME),
                script_text: HOOK_SCRIPT,
                events: CLAUDE_EVENTS,
            },
            IntegrationTarget::Omp => Self {
                config: None,
                script: dir.join("extensions").join(EXTENSION_NAME),
                script_text: OMP_EXTENSION,
                events: &[],
            },
        }
    }
}

/// The hook command: a no-op builtin carrying the marker, then the script.
fn hook_command(target: IntegrationTarget, script: &Path) -> String {
    format!(
        ": {}{}; sh {} {}",
        hooks_json::MARKER,
        name(target),
        shell_quote(&script.to_string_lossy()),
        name(target)
    )
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Reads back what is on disk and sets `report.status` and `report.missing`.
fn finish_status(layout: &Layout, report: &mut Report) -> Result<(), CliError> {
    let mut missing = Vec::new();
    let mut present = 0usize;

    match read_optional(&layout.script)? {
        Some(text) if text == layout.script_text => present += 1,
        Some(text) if is_managed(&text) => {
            present += 1;
            missing.push(format!("{} is outdated", layout.script.display()));
        }
        Some(_) => missing.push(format!(
            "{} exists but is not Heeler's",
            layout.script.display()
        )),
        None => missing.push(format!("{} is missing", layout.script.display())),
    }
    if let Some(config) = &layout.config {
        let doc = read_json(config)?.unwrap_or_else(|| Value::Object(Default::default()));
        let installed = hooks_json::installed_events(&doc, layout.events);
        present += installed.len();
        for event in layout.events {
            if !installed.contains(event) {
                missing.push(format!("hook {event} is missing in {}", config.display()));
            }
        }
    }
    report.status = if missing.is_empty() {
        Status::Installed
    } else if present == 0 {
        Status::NotInstalled
    } else {
        Status::Partial
    };
    report.missing = if report.status == Status::Partial {
        missing
    } else {
        Vec::new()
    };
    Ok(())
}

// ---- files ------------------------------------------------------------------------------------

fn is_managed(text: &str) -> bool {
    text.lines().take(3).any(|line| line.contains(FILE_MARKER))
}

fn io_error(path: &Path, error: io::Error) -> CliError {
    CliError::Failed(format!("{}: {error}", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<String>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, e)),
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, CliError> {
    read_optional(path)?
        .map(|text| parse_json(path, &text))
        .transpose()
}

/// An empty file counts as an empty document.
fn parse_json(path: &Path, text: &str) -> Result<Value, CliError> {
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(text).map_err(|e| {
        CliError::Failed(format!(
            "{} is not valid JSON ({e}); not touching it",
            path.display()
        ))
    })
}

/// Writes a whole file Heeler owns (script, extension). Refuses to replace one that is not.
fn write_managed(path: &Path, text: &str, mode: u32, report: &mut Report) -> Result<(), CliError> {
    if let Some(existing) = read_optional(path)? {
        if existing == text {
            return Ok(());
        }
        if !is_managed(&existing) {
            return Err(CliError::Failed(format!(
                "{} exists and was not written by Heeler; move it away first",
                path.display()
            )));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    write_atomic(path, text.as_bytes(), Some(mode))?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

fn remove_managed(path: &Path, report: &mut Report) -> Result<(), CliError> {
    let Some(existing) = read_optional(path)? else {
        return Ok(());
    };
    if !is_managed(&existing) {
        return Ok(());
    }
    fs::remove_file(path).map_err(|e| io_error(path, e))?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

/// An edit of an agent's JSON configuration, worked out but not yet written.
struct ConfigEdit {
    path: PathBuf,
    /// The file as it is now; `None` if it does not exist.
    original: Option<String>,
    doc: Value,
    changed: bool,
}

/// Applies `edit` to the JSON document at `path` (an empty one if the file is missing)
/// without writing anything.
fn plan_config(
    path: &Path,
    edit: impl FnOnce(&mut Value) -> Result<bool, hooks_json::EditError>,
) -> Result<ConfigEdit, CliError> {
    let original = read_optional(path)?;
    let mut doc = match &original {
        Some(text) => parse_json(path, text)?,
        None => Value::Object(serde_json::Map::new()),
    };
    let changed = edit(&mut doc)
        .map_err(|e| CliError::Failed(format!("{}: {e}; not touching it", path.display())))?;
    Ok(ConfigEdit {
        path: path.to_owned(),
        original,
        doc,
        changed,
    })
}

impl ConfigEdit {
    /// Backs the file up and writes the document with the file's own indentation. An edit that
    /// changed nothing writes nothing; a document an uninstall leaves empty removes the file.
    fn apply(self, report: &mut Report) -> Result<(), CliError> {
        let Self {
            path,
            original,
            doc,
            changed,
        } = self;
        if !changed {
            return Ok(());
        }
        if let Some(text) = &original {
            report.backups.push(backup(&path, text)?);
        }
        if doc.as_object().is_some_and(serde_json::Map::is_empty) && original.is_some() {
            fs::remove_file(&path).map_err(|e| io_error(&path, e))?;
        } else {
            let text = render_json(&doc, original.as_deref())?;
            let mode = fs::metadata(&path).ok().map(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o777
            });
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
            }
            write_atomic(&path, text.as_bytes(), mode)?;
        }
        report.changed = true;
        report.files.push(path);
        Ok(())
    }
}

/// The document as text, indented like `original` (two spaces for a new file) with its trailing
/// newline.
fn render_json(doc: &Value, original: Option<&str>) -> Result<String, CliError> {
    use serde::Serialize as _;
    let (indent, newline) = match original {
        Some(text) if !text.trim().is_empty() => (detect_indent(text), text.ends_with('\n')),
        _ => (Some("  ".to_owned()), true),
    };
    let mut out = Vec::new();
    match indent {
        Some(indent) => {
            let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
            let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
            doc.serialize(&mut serializer)
                .map_err(|e| CliError::Failed(e.to_string()))?;
        }
        None => {
            serde_json::to_writer(&mut out, doc).map_err(|e| CliError::Failed(e.to_string()))?
        }
    }
    if newline {
        out.push(b'\n');
    }
    String::from_utf8(out).map_err(|e| CliError::Failed(e.to_string()))
}

/// The indentation of the first indented line; `None` for a document on a single line.
fn detect_indent(text: &str) -> Option<String> {
    let mut lines = text.trim_end().lines();
    lines.next();
    let mut any = false;
    for line in lines {
        any = true;
        let indent = &line[..line.len() - line.trim_start().len()];
        if !indent.is_empty() {
            return Some(indent.to_owned());
        }
    }
    // Several lines but no indentation: keep the default; one line: stay compact.
    any.then(|| "  ".to_owned())
}

/// Copies `path` to a fresh `<name>.heeler-<unix ms>.bak` next to it.
fn backup(path: &Path, contents: &str) -> Result<PathBuf, CliError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 0u32..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = path.with_file_name(format!("{file_name}.heeler-{stamp}{suffix}.bak"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(contents.as_bytes())
                    .map_err(|e| io_error(&candidate, e))?;
                if let Ok(meta) = fs::metadata(path) {
                    let _ = fs::set_permissions(&candidate, meta.permissions());
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(&candidate, e)),
        }
    }
    Err(CliError::Failed(format!(
        "no free backup name next to {}",
        path.display()
    )))
}

/// Replaces `path` through a temporary file in the same directory, so the agent never reads a
/// half-written configuration.
fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), CliError> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.heeler-tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(mode) = mode {
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| io_error(path, e))
}

/// Human text for a report.
pub fn describe(action: &str, report: &Report) -> String {
    let mut text = match (action, report.status) {
        ("install", _) if report.changed => {
            format!("{}: installed in {}", report.target, report.dir.display())
        }
        ("install", _) => format!(
            "{}: already installed in {}",
            report.target,
            report.dir.display()
        ),
        ("uninstall", _) if report.changed => {
            format!("{}: removed from {}", report.target, report.dir.display())
        }
        ("uninstall", _) => format!(
            "{}: nothing to remove in {}",
            report.target,
            report.dir.display()
        ),
        (_, Status::Installed) => {
            format!("{}: installed ({})", report.target, report.dir.display())
        }
        (_, Status::NotInstalled) => {
            format!(
                "{}: not installed ({})",
                report.target,
                report.dir.display()
            )
        }
        (_, Status::Partial) => format!(
            "{}: partially installed ({})",
            report.target,
            report.dir.display()
        ),
    };
    for backup in &report.backups {
        text.push_str(&format!("\n  backup: {}", backup.display()));
    }
    for item in &report.missing {
        text.push_str(&format!("\n  {item}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_and_trailing_newline_follow_the_original() {
        assert_eq!(detect_indent("{\n  \"a\": 1\n}\n").as_deref(), Some("  "));
        assert_eq!(detect_indent("{\n\t\"a\": 1\n}").as_deref(), Some("\t"));
        assert_eq!(
            detect_indent("{\n    \"a\": {\n      \"b\": 1\n    }\n}").as_deref(),
            Some("    ")
        );
        assert_eq!(detect_indent("{\"a\":1}\n"), None);
        assert_eq!(detect_indent("{\n\"a\": 1\n}").as_deref(), Some("  "));
    }

    #[test]
    fn script_paths_are_quoted_for_sh() {
        assert_eq!(
            shell_quote("/home/me/.codex/x.sh"),
            "'/home/me/.codex/x.sh'"
        );
        assert_eq!(shell_quote("/home/o'neil/x.sh"), "'/home/o'\\''neil/x.sh'");
    }

    #[test]
    fn embedded_files_carry_the_ownership_marker() {
        assert!(is_managed(HOOK_SCRIPT));
        assert!(is_managed(OMP_EXTENSION));
        assert!(!is_managed("#!/bin/sh\necho hi\n"));
    }
}
