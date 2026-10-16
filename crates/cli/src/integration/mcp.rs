//! `hutan integration install|uninstall|status <agent> --mcp`: the `hutan mcp` server in the
//! agent's own MCP configuration (ADR 0016).
//!
//! Only on that explicit command, and only Hutan's entry: `mcpServers.hutan` (Claude Code's
//! `.claude.json`, OMP's `mcp.json`) or `[mcp_servers.hutan]` (Codex's `config.toml`), marked by
//! an `env` variable that every agent passes through to the server and that no other tool
//! writes. The file is backed up first; an entry that is not marked is never touched.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, Table, value};

use super::hooks_json::EditError;
use super::{Report, Status, backup, io_error, plan_config, read_optional, write_atomic};
use crate::args::IntegrationTarget;
use crate::error::CliError;

/// The server's name in the agent's configuration.
const SERVER: &str = "hutan";
/// The `env` variable that marks the entry as Hutan's.
const MARKER_KEY: &str = "HUTAN_MCP_ENTRY";
const MARKER_VALUE: &str = "hutan-managed";
/// A tool call may wait for the user's approval: hosts must not give up on it early
/// (`[control] approval_timeout_secs` defaults to 600).
const TOOL_TIMEOUT_SECS: u64 = 700;

/// Where the agent keeps its MCP servers.
pub fn config_path(target: IntegrationTarget, dir: &Path) -> PathBuf {
    match target {
        IntegrationTarget::Codex => dir.join("config.toml"),
        // `~/.claude.json` sits next to `~/.claude/`; with CLAUDE_CONFIG_DIR (or --dir) it is
        // inside the directory.
        IntegrationTarget::Claude => match (dir.file_name(), dir.parent()) {
            (Some(name), Some(parent)) if name == ".claude" => parent.join(".claude.json"),
            _ => dir.join(".claude.json"),
        },
        IntegrationTarget::Omp => dir.join("mcp.json"),
    }
}

fn json_entry(target: IntegrationTarget, hutan_bin: &Path) -> Value {
    let mut entry = json!({
        "type": "stdio",
        "command": hutan_bin.to_string_lossy(),
        "args": ["mcp"],
        "env": {MARKER_KEY: MARKER_VALUE},
    });
    if target == IntegrationTarget::Omp {
        // OMP counts milliseconds.
        entry["timeout"] = json!(TOOL_TIMEOUT_SECS * 1000);
    }
    entry
}

fn is_ours_json(entry: &Value) -> bool {
    entry
        .pointer(&format!("/env/{MARKER_KEY}"))
        .and_then(Value::as_str)
        == Some(MARKER_VALUE)
}

fn report(target: IntegrationTarget, dir: &Path) -> Report {
    Report::new(target, dir)
}

pub fn install(
    target: IntegrationTarget,
    dir: &Path,
    hutan_bin: &Path,
) -> Result<Report, CliError> {
    let mut report = report(target, dir);
    let path = config_path(target, dir);
    match target {
        IntegrationTarget::Codex => install_toml(&path, hutan_bin, &mut report)?,
        _ => {
            let entry = json_entry(target, hutan_bin);
            plan_config(&path, |doc| set_json_entry(doc, &entry))?.apply(&mut report)?;
        }
    }
    finish(target, dir, hutan_bin, &mut report)?;
    Ok(report)
}

pub fn uninstall(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let mut report = report(target, dir);
    let path = config_path(target, dir);
    match target {
        IntegrationTarget::Codex => uninstall_toml(&path, &mut report)?,
        _ => plan_config(&path, remove_json_entry)?.apply(&mut report)?,
    }
    finish(target, dir, Path::new(""), &mut report)?;
    Ok(report)
}

pub fn status(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let mut report = report(target, dir);
    finish(target, dir, Path::new(""), &mut report)?;
    Ok(report)
}

/// Sets `report.status` from what is on disk. A recorded `hutan_bin` that differs from the
/// entry's command makes an installed entry `partial` (the binary moved); without one only
/// presence is checked.
fn finish(
    target: IntegrationTarget,
    dir: &Path,
    hutan_bin: &Path,
    report: &mut Report,
) -> Result<(), CliError> {
    let path = config_path(target, dir);
    report.missing.clear();
    let found = match target {
        IntegrationTarget::Codex => read_toml_entry(&path)?,
        _ => read_json_entry(&path)?,
    };
    report.status = match found {
        None => {
            report
                .missing
                .push(format!("no `hutan` MCP server in {}", path.display()));
            Status::NotInstalled
        }
        Some(Found::Foreign) => {
            report.missing.push(format!(
                "{} has a `hutan` MCP server that is not Hutan's",
                path.display()
            ));
            Status::NotInstalled
        }
        Some(Found::Ours { command }) => {
            if !hutan_bin.as_os_str().is_empty() && Path::new(&command) != hutan_bin {
                report.missing.push(format!(
                    "the entry runs {command}, not {}",
                    hutan_bin.display()
                ));
                Status::Partial
            } else {
                Status::Installed
            }
        }
    };
    if report.status != Status::Partial {
        // Only a partial entry has something to report as missing.
        let foreign = report
            .missing
            .first()
            .cloned()
            .filter(|m| m.contains("not Hutan's"));
        report.missing = foreign.into_iter().collect();
    }
    Ok(())
}

enum Found {
    Ours { command: String },
    Foreign,
}

fn read_json_entry(path: &Path) -> Result<Option<Found>, CliError> {
    let Some(text) = read_optional(path)? else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let doc: Value = serde_json::from_str(&text)
        .map_err(|e| CliError::Failed(format!("{}: {e}", path.display())))?;
    Ok(doc.pointer(&format!("/mcpServers/{SERVER}")).map(|entry| {
        if is_ours_json(entry) {
            Found::Ours {
                command: entry
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }
        } else {
            Found::Foreign
        }
    }))
}

fn set_json_entry(doc: &mut Value, entry: &Value) -> Result<bool, EditError> {
    let Some(root) = doc.as_object_mut() else {
        return Err(EditError::NotAnObject);
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(servers) = servers.as_object_mut() else {
        return Err(EditError::UnexpectedShape("mcpServers".to_owned()));
    };
    match servers.get(SERVER) {
        Some(existing) if !is_ours_json(existing) => Err(EditError::Refused(format!(
            "`mcpServers.{SERVER}` exists and is not Hutan's; remove or rename it first"
        ))),
        Some(existing) if existing == entry => Ok(false),
        _ => {
            servers.insert(SERVER.to_owned(), entry.clone());
            Ok(true)
        }
    }
}

fn remove_json_entry(doc: &mut Value) -> Result<bool, EditError> {
    let Some(servers) = doc.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if !servers.get(SERVER).is_some_and(is_ours_json) {
        return Ok(false);
    }
    servers.remove(SERVER);
    if servers.is_empty()
        && let Some(root) = doc.as_object_mut()
    {
        root.remove("mcpServers");
    }
    Ok(true)
}

// ---- Codex: config.toml ---------------------------------------------------------------------

fn read_toml(path: &Path) -> Result<(Option<String>, DocumentMut), CliError> {
    let original = read_optional(path)?;
    let doc = match &original {
        Some(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| CliError::Failed(format!("{}: {e}; not touching it", path.display())))?,
        None => DocumentMut::new(),
    };
    Ok((original, doc))
}

fn toml_entry_is_ours(table: &dyn toml_edit::TableLike) -> bool {
    table
        .get("env")
        .and_then(Item::as_table_like)
        .and_then(|env| env.get(MARKER_KEY))
        .and_then(Item::as_str)
        == Some(MARKER_VALUE)
}

fn read_toml_entry(path: &Path) -> Result<Option<Found>, CliError> {
    let (_, doc) = read_toml(path)?;
    let Some(entry) = doc
        .get("mcp_servers")
        .and_then(Item::as_table_like)
        .and_then(|servers| servers.get(SERVER))
        .and_then(Item::as_table_like)
    else {
        return Ok(None);
    };
    Ok(Some(if toml_entry_is_ours(entry) {
        Found::Ours {
            command: entry
                .get("command")
                .and_then(Item::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    } else {
        Found::Foreign
    }))
}

fn toml_entry(hutan_bin: &Path) -> Table {
    let mut table = Table::new();
    table.insert("command", value(hutan_bin.to_string_lossy().as_ref()));
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    table.insert("args", value(args));
    table.insert(
        "tool_timeout_sec",
        value(i64::try_from(TOOL_TIMEOUT_SECS).unwrap_or(700)),
    );
    let mut env = Table::new();
    env.insert(MARKER_KEY, value(MARKER_VALUE));
    table.insert("env", Item::Table(env));
    table
}

fn install_toml(path: &Path, hutan_bin: &Path, report: &mut Report) -> Result<(), CliError> {
    let (original, mut doc) = read_toml(path)?;
    let wanted = toml_entry(hutan_bin);
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_like_mut()
        .ok_or_else(|| {
            CliError::Failed(format!(
                "{}: `mcp_servers` is not a table; not touching it",
                path.display()
            ))
        })?;
    if let Some(existing) = servers.get(SERVER) {
        let ours = existing.as_table_like().is_some_and(toml_entry_is_ours);
        if !ours {
            return Err(CliError::Failed(format!(
                "{}: `mcp_servers.{SERVER}` exists and is not Hutan's; remove or rename it \
                 first. Nothing was changed",
                path.display()
            )));
        }
        if existing.to_string().trim() == Item::Table(wanted.clone()).to_string().trim() {
            return Ok(());
        }
    }
    servers.insert(SERVER, Item::Table(wanted));
    write_toml(path, original.as_deref(), &doc, report)
}

fn uninstall_toml(path: &Path, report: &mut Report) -> Result<(), CliError> {
    let (original, mut doc) = read_toml(path)?;
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };
    if !servers
        .get(SERVER)
        .and_then(Item::as_table_like)
        .is_some_and(toml_entry_is_ours)
    {
        return Ok(());
    }
    servers.remove(SERVER);
    if servers.is_empty() {
        doc.remove("mcp_servers");
    }
    write_toml(path, original.as_deref(), &doc, report)
}

fn write_toml(
    path: &Path,
    original: Option<&str>,
    doc: &DocumentMut,
    report: &mut Report,
) -> Result<(), CliError> {
    if let Some(text) = original {
        report.backups.push(backup(path, text)?);
    }
    // A file that held only Hutan's entry goes away with it, like the JSON configurations.
    if doc.to_string().trim().is_empty() && original.is_some() {
        fs::remove_file(path).map_err(|e| io_error(path, e))?;
        report.changed = true;
        report.files.push(path.to_owned());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    let mode = fs::metadata(path).ok().map(|meta| {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    });
    write_atomic(path, doc.to_string().as_bytes(), mode)?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}
