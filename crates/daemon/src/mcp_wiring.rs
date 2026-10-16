//! `hutan mcp` as an MCP server of the agents Hutan starts (ADR 0016, protocol 0.21).
//!
//! The chat manager owns one [`McpWiring`] (the `hutan` executable, the daemon's socket, and
//! `[control] mcp`) and hands [`McpWiring::server_for`] to whatever starts an agent: Codex gets
//! it as `-c mcp_servers.hutan…` overrides ([`McpServerSpec::codex_overrides`]); the ACP path of
//! The ACP harness path puts [`McpServerSpec::acp_server`] into its `mcpServers` list. The server runs inside the agent's
//! session, so it carries `HUTAN_SESSION_ID` and its requests are agent-originated.

use std::path::PathBuf;

use hutan_core::SessionId;

/// The name the server is registered under in the agent's configuration.
pub const SERVER_NAME: &str = "hutan";

/// How to start one MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// What the daemon needs to describe its own MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpWiring {
    /// `[control] mcp`.
    pub enabled: bool,
    /// The `hutan` CLI (the one sessions get as `HUTAN_BIN`).
    pub hutan_bin: PathBuf,
    /// The daemon's own socket, so the server reaches this daemon whatever the environment.
    pub socket: PathBuf,
}

impl McpWiring {
    /// The server for the agent of `session`; `None` under `[control] mcp = false`.
    pub fn server_for(&self, session: SessionId) -> Option<McpServerSpec> {
        self.enabled.then(|| McpServerSpec {
            name: SERVER_NAME.to_owned(),
            command: self.hutan_bin.clone(),
            args: vec!["mcp".to_owned()],
            env: vec![
                ("HUTAN_SESSION_ID".to_owned(), session.to_string()),
                (
                    "HUTAN_SOCKET".to_owned(),
                    self.socket.to_string_lossy().into_owned(),
                ),
            ],
        })
    }
}

impl McpServerSpec {
    /// The ACP `McpServer` object (stdio form) for `mcpServers` of `session/new` and
    /// `session/load`: `env` is a list of `{name, value}`.
    pub fn acp_server(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "command": self.command.to_string_lossy(),
            "args": self.args,
            "env": self
                .env
                .iter()
                .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
                .collect::<Vec<_>>(),
        })
    }

    /// Arguments for `codex app-server` that register this server for the session
    /// (`-c mcp_servers.<name>.command=…`, `.args`, `.env`); values are TOML.
    pub fn codex_overrides(&self) -> Vec<String> {
        let key = |field: &str| format!("mcp_servers.{}.{field}", self.name);
        let command = toml::Value::String(self.command.to_string_lossy().into_owned());
        let args = toml::Value::Array(self.args.iter().cloned().map(toml::Value::String).collect());
        let env = toml::Value::Table(
            self.env
                .iter()
                .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
                .collect(),
        );
        let mut out = Vec::new();
        for (field, value) in [("command", command), ("args", args), ("env", env)] {
            out.push("-c".to_owned());
            out.push(format!("{}={}", key(field), value));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiring(enabled: bool) -> McpWiring {
        McpWiring {
            enabled,
            hutan_bin: PathBuf::from("/Applications/Hutan.app/Contents/MacOS/hutan"),
            socket: PathBuf::from("/tmp/h/hutand.sock"),
        }
    }

    #[test]
    fn a_disabled_wiring_names_no_server() {
        assert_eq!(wiring(false).server_for(SessionId::new()), None);
    }

    #[test]
    fn the_server_runs_hutan_mcp_inside_the_session() {
        let session = SessionId::new();
        let spec = wiring(true).server_for(session).unwrap();
        assert_eq!(spec.name, "hutan");
        assert_eq!(spec.args, ["mcp"]);
        assert!(
            spec.env
                .contains(&("HUTAN_SESSION_ID".to_owned(), session.to_string()))
        );
        assert!(
            spec.env
                .contains(&("HUTAN_SOCKET".to_owned(), "/tmp/h/hutand.sock".to_owned()))
        );
    }

    #[test]
    fn an_acp_harness_gets_the_stdio_object_with_the_sessions_environment() {
        let session = SessionId::new();
        let object = wiring(true).server_for(session).unwrap().acp_server();
        assert_eq!(object["name"], "hutan");
        assert_eq!(object["args"], serde_json::json!(["mcp"]));
        let env = object["env"].as_array().unwrap();
        assert!(
            env.iter()
                .any(|e| e["name"] == "HUTAN_SESSION_ID"
                    && e["value"] == session.to_string().as_str())
        );
    }

    #[test]
    fn codex_gets_toml_overrides() {
        let session = SessionId::new();
        let spec = wiring(true).server_for(session).unwrap();
        let args = spec.codex_overrides();
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], "-c");
        assert_eq!(
            args[1],
            "mcp_servers.hutan.command=\"/Applications/Hutan.app/Contents/MacOS/hutan\""
        );
        assert_eq!(args[3], "mcp_servers.hutan.args=[\"mcp\"]");
        // Every override is valid TOML on its own.
        for pair in args.chunks(2) {
            let parsed: Result<toml::Table, _> = pair[1].parse();
            assert!(parsed.is_ok(), "{}: {parsed:?}", pair[1]);
        }
        assert!(args[5].starts_with("mcp_servers.hutan.env="));
        assert!(args[5].contains(&session.to_string()));
    }
}
