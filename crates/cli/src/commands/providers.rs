//! `staffy provider add|key|ls|models|rm|discover`: model providers for direct chat.

use std::io::{IsTerminal as _, Read as _, Write as _};

use serde_json::json;
use staffy_protocol::provider::{
    DiscoveredProvider, ProviderAdd, ProviderInfo, ProviderList, ProviderModels, ProviderResult,
};

use super::Ctx;
use crate::args::{
    ProviderAddArgs, ProviderCommand, ProviderDiscoverArgs, ProviderIdArgs, ProviderKeyArgs,
};
use crate::error::CliError;

/// The longest key accepted from stdin.
const MAX_SECRET_BYTES: u64 = 16 * 1024;

impl Ctx {
    pub(super) async fn provider(&self, command: ProviderCommand) -> Result<(), CliError> {
        match command {
            ProviderCommand::Add(args) => self.provider_add(args).await,
            ProviderCommand::Key(args) => self.provider_key(args).await,
            ProviderCommand::Ls => self.provider_ls().await,
            ProviderCommand::Models(args) => self.provider_models(args).await,
            ProviderCommand::Rm(args) => self.provider_rm(args).await,
            ProviderCommand::Discover(args) => self.provider_discover(args).await,
        }
    }

    async fn provider_add(&self, args: ProviderAddArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let provider = client
            .provider_add(ProviderAdd {
                name: args.name.unwrap_or_else(|| args.kind.clone()),
                kind: args.kind,
                base_url: args.base_url,
                default_model: args.default_model,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let result = ProviderResult { provider };
        self.print(&result, || {
            let provider = &result.provider;
            let hint = if needs_key(provider) {
                format!(
                    "\nstore its API key with: staffy provider key {}",
                    provider.id
                )
            } else {
                String::new()
            };
            format!(
                "provider {} ({} at {}){hint}",
                provider.id, provider.kind, provider.base_url
            )
        })
    }

    async fn provider_key(&self, args: ProviderKeyArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if args.clear {
            let provider = client
                .provider_clear_credential(&args.provider)
                .await
                .map_err(|e| self.fail(e))?;
            let result = ProviderResult { provider };
            return self.print(&result, || {
                format!("removed the key of provider {}", result.provider.id)
            });
        }
        let secret = read_secret(args.from_env.as_deref(), &args.provider)?;
        let provider = client
            .provider_set_credential(&args.provider, secret)
            .await
            .map_err(|e| self.fail(e))?;
        // The secret is not echoed: the daemon's answer only says that there is one.
        let result = ProviderResult { provider };
        self.print(&result, || {
            format!("stored the key of provider {}", result.provider.id)
        })
    }

    async fn provider_ls(&self) -> Result<(), CliError> {
        let client = self.connect().await?;
        let providers = client.provider_list().await.map_err(|e| self.fail(e))?;
        let list = ProviderList { providers };
        self.print(&list, || provider_table(&list.providers))
    }

    async fn provider_models(&self, args: ProviderIdArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let models = client
            .provider_models(&args.provider)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&models, || model_table(&models))
    }

    async fn provider_rm(&self, args: ProviderIdArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        client
            .provider_remove(&args.provider)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&json!({"provider_id": args.provider}), || {
            format!("removed provider {}", args.provider)
        })
    }

    async fn provider_discover(&self, args: ProviderDiscoverArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let found = client.provider_discover().await.map_err(|e| self.fail(e))?;
        let mut added = Vec::new();
        if args.add {
            for server in found.iter().filter(|d| d.provider_id.is_none()) {
                let provider = client
                    .provider_add(ProviderAdd {
                        kind: server.kind.clone(),
                        name: server.name.clone(),
                        base_url: Some(server.base_url.clone()),
                        default_model: None,
                    })
                    .await
                    .map_err(|e| self.fail(e))?;
                added.push(provider);
            }
        }
        self.print(&json!({"found": found, "added": added}), || {
            discover_text(&found, &added)
        })
    }
}

fn needs_key(provider: &ProviderInfo) -> bool {
    matches!(provider.kind.as_str(), "openai" | "anthropic") && !provider.has_credential
}

fn provider_table(providers: &[ProviderInfo]) -> String {
    if providers.is_empty() {
        return "no providers (add one with `staffy provider add`, or look for local ones with \
                `staffy provider discover`)"
            .to_owned();
    }
    let mut rows = vec![[
        "ID".to_owned(),
        "KIND".to_owned(),
        "KEY".to_owned(),
        "BASE URL".to_owned(),
        "DEFAULT MODEL".to_owned(),
    ]];
    for p in providers {
        rows.push([
            p.id.clone(),
            p.kind.clone(),
            if p.has_credential { "yes" } else { "-" }.to_owned(),
            p.base_url.clone(),
            p.default_model.clone().unwrap_or_else(|| "-".to_owned()),
        ]);
    }
    aligned(&rows)
}

fn model_table(models: &ProviderModels) -> String {
    if models.models.is_empty() {
        return format!("provider {} offers no models", models.provider_id);
    }
    models
        .models
        .iter()
        .map(|m| {
            let marker = if m.is_default { " (default)" } else { "" };
            if m.description.is_empty() {
                format!("{}{marker}", m.id)
            } else {
                format!("{}{marker}\t{}", m.id, m.description)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn discover_text(found: &[DiscoveredProvider], added: &[ProviderInfo]) -> String {
    if found.is_empty() {
        return "no local model server found (looked for Ollama on 127.0.0.1:11434 and LM \
                Studio on 127.0.0.1:1234)"
            .to_owned();
    }
    let mut lines = Vec::new();
    for server in found {
        let models = match server.models.len() {
            0 => "no models".to_owned(),
            1 => "1 model".to_owned(),
            n => format!("{n} models"),
        };
        let status = match (
            &server.provider_id,
            added.iter().find(|p| p.base_url == server.base_url),
        ) {
            (Some(id), _) => format!("configured as {id}"),
            (None, Some(provider)) => format!("added as {}", provider.id),
            (None, None) => format!(
                "add it with: staffy provider add {} --name '{}' --base-url {}",
                server.kind, server.name, server.base_url
            ),
        };
        lines.push(format!(
            "{} at {}: {models}; {status}",
            server.name, server.base_url
        ));
    }
    lines.join("\n")
}

/// Left-aligned columns separated by two spaces.
fn aligned(rows: &[[String; 5]]) -> String {
    let mut widths = [0usize; 5];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                line.push_str(cell);
                if i + 1 < row.len() {
                    line.push_str(&" ".repeat(widths[i] - cell.chars().count() + 2));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The API key: from `--from-env`, from a pipe on stdin, or typed at a prompt that does not
/// echo. Surrounding whitespace (the newline) is not part of it.
fn read_secret(from_env: Option<&str>, provider: &str) -> Result<String, CliError> {
    let secret = match from_env {
        Some(name) => std::env::var(name)
            .map_err(|_| CliError::Failed(format!("environment variable {name} is not set")))?,
        None if std::io::stdin().is_terminal() => prompt_hidden(provider)?,
        None => {
            let mut text = String::new();
            std::io::stdin()
                .take(MAX_SECRET_BYTES)
                .read_to_string(&mut text)?;
            text
        }
    };
    let secret = secret.trim().to_owned();
    if secret.is_empty() {
        return Err(CliError::Failed("the key is empty".to_owned()));
    }
    Ok(secret)
}

/// Reads a line from the terminal in raw mode, so nothing is echoed.
fn prompt_hidden(provider: &str) -> Result<String, CliError> {
    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    eprint!("API key for {provider} (input is hidden): ");
    std::io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let _raw = RawMode;
    let mut bytes: Vec<u8> = Vec::new();
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    let outcome = loop {
        if stdin.read(&mut byte)? == 0 {
            break Ok(());
        }
        match byte[0] {
            b'\r' | b'\n' => break Ok(()),
            // ctrl-c, ctrl-d
            0x03 | 0x04 => break Err(CliError::Failed("cancelled".to_owned())),
            0x7f | 0x08 => {
                // Remove the last character, not just its last byte.
                while bytes.pop().is_some_and(|b| b & 0xC0 == 0x80) {}
            }
            b => {
                bytes.push(b);
                if bytes.len() as u64 > MAX_SECRET_BYTES {
                    break Err(CliError::Failed("the key is too long".to_owned()));
                }
            }
        }
    };
    drop(_raw);
    eprintln!();
    outcome?;
    String::from_utf8(bytes).map_err(|_| CliError::Failed("the key is not valid text".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, kind: &str, key: bool) -> ProviderInfo {
        ProviderInfo {
            id: id.to_owned(),
            kind: kind.to_owned(),
            name: id.to_owned(),
            base_url: "http://h".to_owned(),
            default_model: None,
            has_credential: key,
            created_at_ms: 0,
        }
    }

    #[test]
    fn the_table_aligns_its_columns_and_says_where_keys_are() {
        let text = provider_table(&[
            provider("a", "openai", true),
            provider("long-id", "ollama", false),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("ID       KIND    KEY"), "{text}");
        assert!(lines[1].starts_with("a        openai  yes"), "{text}");
        assert!(lines[2].starts_with("long-id  ollama  -  "), "{text}");
    }

    #[test]
    fn only_key_providers_without_a_key_get_the_hint() {
        assert!(needs_key(&provider("a", "openai", false)));
        assert!(!needs_key(&provider("a", "openai", true)));
        assert!(!needs_key(&provider("a", "ollama", false)));
    }
}
