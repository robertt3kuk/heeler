//! Assistant sessions and their context providers (protocol 0.12, `docs/assistant.md`).
//!
//! An assistant is a direct chat (`crate::direct`) whose session has `chat.agent = "assistant"`
//! and a default system prompt. This service finds or creates the default assistant of a scope
//! (`assistant.open`), lists them (`assistant.list`), and turns the attachments of a
//! `chat.send` into the text the message carries ([`AssistantService::prepare`]).
//!
//! Context is read-only, explicit and bounded. Nothing is read that is not attached; an
//! attachment the settings do not allow is refused with `forbidden` before anything is read;
//! every text is redacted before it is cut to its share of `assistant.context_budget_chars`
//! and journaled with the message. Files are only read below the roots of the session's
//! workspace ([`crate::context_file`]); terminal text comes from the terminal hub (the
//! `terminal.block_text` path), Hutan chats from their journal, transcripts of other tools and
//! searches from the history index.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use hutan_core::agent::MessageRole;
use hutan_core::{AgentEvent, AgentEventKind, SessionId, TerminalId, WorkspaceId};
use hutan_index::{Role, SessionTextQuery, Source};
use hutan_protocol::assistant::{
    AGENT_ASSISTANT, ATTACHMENTS_MAX, AssistantEntry, AssistantList, AssistantListRequest,
    AssistantOpen, AssistantScope, Attachment, AttachmentKind, LineRange,
};
use hutan_protocol::blocks::{BlockPart, BlockState, TerminalBlockText, TerminalBlocks};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::history::HistorySearch;
use hutan_protocol::messages::{SessionInfo, SessionListFilter, WorkspaceInfo, WorkspaceKind};
use hutan_storage::Storage;
use tokio::sync::Mutex;

use crate::chat::is_gateway_agent;
use crate::config::{AssistantSettings, ContextPermissions, gating_setting};
use crate::context::{
    self, Attached, Bounded, Keep, Message, Part, REDACT_SLACK, last_turns, merge_chunks, one_line,
    redact_and_bound,
};
use crate::context_file::{FileContext, read_file};
use crate::history::HistoryService;
use crate::registry::RegistryHandle;
use crate::terminals::TerminalHub;

/// Most journal rows read to rebuild a conversation for a transcript attachment.
const JOURNAL_ROWS: u32 = 4000;
/// Most indexed rows read for a transcript attachment.
const INDEX_ROWS: usize = 4000;
/// Longest snippet of a search hit in a `search` attachment, in characters.
const SNIPPET_CHARS: usize = 300;

/// The system prompt of an assistant created by `assistant.open`.
pub const SYSTEM_PROMPT: &str = "You are Hutan's assistant, built into a developer's terminal \
workspace. Answer concisely and concretely. A message may end with attached context between \
`--- attached context` markers: read-only excerpts the user chose to share (terminal output, \
files, conversations with coding agents, search results), each introduced by a bracketed \
header that says what it is and whether it was cut. Rely on that context and on the \
conversation; if the answer needs something that was not attached, say what to attach \
instead of guessing. You cannot run commands or change files.";

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(code, message)
}

fn internal(error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, error.to_string())
}

/// Runs assistants and resolves attachments.
#[derive(Debug)]
pub struct AssistantService {
    settings: AssistantSettings,
    registry: RegistryHandle,
    hub: Arc<TerminalHub>,
    history: Arc<HistoryService>,
    storage: Storage,
    /// Two `assistant.open` of one scope at once must not create two defaults.
    open_lock: Mutex<()>,
}

/// What the session a message is sent in may read, and where.
#[derive(Debug)]
struct Access {
    scope: AssistantScope,
    permissions: ContextPermissions,
    workspace_name: String,
    /// The setting table the permissions came from, for the refusal message.
    settings_key: Option<String>,
    roots: Vec<PathBuf>,
}

impl AssistantService {
    pub fn new(
        settings: AssistantSettings,
        registry: RegistryHandle,
        hub: Arc<TerminalHub>,
        history: Arc<HistoryService>,
        storage: Storage,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            registry,
            hub,
            history,
            storage,
            open_lock: Mutex::new(()),
        })
    }

    async fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, ErrorBody> {
        Ok(self.registry.list_workspaces().await?.workspaces)
    }

    /// The assistant sessions of the daemon, oldest first.
    async fn assistants(&self) -> Result<Vec<SessionInfo>, ErrorBody> {
        let mut sessions: Vec<SessionInfo> = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions
            .into_iter()
            .filter(|s| s.chat.as_ref().is_some_and(|c| c.agent == AGENT_ASSISTANT))
            .collect();
        // Stable: sessions created in the same millisecond keep the registry's creation order
        // (ULIDs of one millisecond are random, so the id cannot break the tie).
        sessions.sort_by_key(|s| s.created_at_ms);
        Ok(sessions)
    }

    fn scope_of(workspace: &WorkspaceInfo) -> AssistantScope {
        if workspace.kind == WorkspaceKind::Home {
            AssistantScope::Global
        } else {
            AssistantScope::Workspace
        }
    }

    // assistant.open / assistant.list -------------------------------------------------------

    /// The default assistant of a scope (the oldest of its workspace), created on first use. A
    /// stopped default is started again. With `new`, one more assistant is created.
    pub async fn open(&self, req: AssistantOpen) -> Result<SessionInfo, ErrorBody> {
        let _one_at_a_time = self.open_lock.lock().await;
        let workspaces = self.workspaces().await?;
        let workspace = match req.scope {
            AssistantScope::Global => workspaces
                .iter()
                .find(|w| w.kind == WorkspaceKind::Home)
                .ok_or_else(|| internal("the daemon has no Home workspace"))?,
            AssistantScope::Workspace => {
                let id = req.workspace_id.ok_or_else(|| {
                    error(
                        ErrorCode::BadRequest,
                        "scope `workspace` needs a `workspace_id`",
                    )
                })?;
                let workspace = workspaces.iter().find(|w| w.id == id).ok_or_else(|| {
                    error(ErrorCode::NotFound, format!("workspace {id} not found"))
                })?;
                if workspace.kind == WorkspaceKind::Home {
                    return Err(error(
                        ErrorCode::BadRequest,
                        "the Home workspace has the global assistant: use scope `global`",
                    ));
                }
                workspace
            }
        };
        let existing: Vec<SessionInfo> = self
            .assistants()
            .await?
            .into_iter()
            .filter(|s| s.workspace_id == workspace.id)
            .collect();
        if !req.new
            && let Some(default) = existing.first()
        {
            if default.state.is_live() {
                return Ok(default.clone());
            }
            return self.registry.restart_session(default.id).await;
        }
        self.create(workspace, existing.len()).await
    }

    async fn create(
        &self,
        workspace: &WorkspaceInfo,
        existing: usize,
    ) -> Result<SessionInfo, ErrorBody> {
        let provider = self.settings.provider.clone().ok_or_else(|| {
            error(
                ErrorCode::InvalidState,
                "no assistant provider is configured: set `provider` (and `model`) under \
                 `[assistant]` in daemon.toml to one of `hutan provider ls`, then restart the daemon",
            )
        })?;
        let cwd = workspace
            .roots
            .first()
            .cloned()
            .ok_or_else(|| internal(format!("workspace {} has no folder", workspace.name)))?;
        let mut title = match Self::scope_of(workspace) {
            AssistantScope::Global => "Assistant".to_owned(),
            AssistantScope::Workspace => format!("Assistant · {}", workspace.name),
        };
        if existing > 0 {
            title.push_str(&format!(" {}", existing + 1));
        }
        self.registry
            .create_chat(ChatCreate {
                workspace_id: Some(workspace.id),
                cwd,
                agent: AGENT_ASSISTANT.to_owned(),
                model: self.settings.model.clone(),
                approval_policy: None,
                sandbox: None,
                title: Some(title),
                provider_id: Some(provider),
                system_prompt: Some(SYSTEM_PROMPT.to_owned()),
            })
            .await
    }

    /// The assistant sessions, oldest first, with the scope of each and which is the default.
    pub async fn list(&self, req: AssistantListRequest) -> Result<AssistantList, ErrorBody> {
        let workspaces = self.workspaces().await?;
        let mut seen: Vec<WorkspaceId> = Vec::new();
        let mut assistants = Vec::new();
        for session in self.assistants().await? {
            let Some(workspace) = workspaces.iter().find(|w| w.id == session.workspace_id) else {
                continue;
            };
            let default = !seen.contains(&workspace.id);
            seen.push(workspace.id);
            let scope = Self::scope_of(workspace);
            if req.scope.is_some_and(|wanted| wanted != scope)
                || req
                    .workspace_id
                    .is_some_and(|wanted| wanted != workspace.id)
            {
                continue;
            }
            assistants.push(AssistantEntry {
                scope,
                workspace_id: workspace.id,
                default,
                session,
            });
        }
        Ok(AssistantList { assistants })
    }

    // Attachments ---------------------------------------------------------------------------

    /// Resolves the attachments of a message sent in `session` into the text it carries, or
    /// `None` when there are none. Checked in full before anything is read: bounds, the kind of
    /// session, and the permissions of its scope. The first attachment that cannot be resolved
    /// fails the whole message (nothing is sent).
    pub async fn prepare(
        &self,
        session: SessionId,
        attachments: &[Attachment],
    ) -> Result<Option<Attached>, ErrorBody> {
        if attachments.is_empty() {
            return Ok(None);
        }
        if attachments.len() > ATTACHMENTS_MAX {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "{} attachments; at most {ATTACHMENTS_MAX} are allowed",
                    attachments.len()
                ),
            ));
        }
        for attachment in attachments {
            context::check_bounds(attachment)
                .map_err(|message| error(ErrorCode::BadRequest, message))?;
        }
        let access = self.access(session).await?;
        for attachment in attachments {
            if !access.permissions.allows(attachment.kind()) {
                return Err(self.forbidden(&access, attachment.kind()));
            }
        }
        let share = context::share(self.settings.context_budget_chars, attachments.len());
        let mut parts = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            parts.push(self.resolve(&access, attachment, share).await?);
        }
        Ok(Some(context::assemble(&parts)))
    }

    async fn access(&self, session: SessionId) -> Result<Access, ErrorBody> {
        let info = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions
            .into_iter()
            .find(|s| s.id == session)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("session {session} not found")))?;
        let Some(chat) = &info.chat else {
            return Err(error(
                ErrorCode::BadRequest,
                format!("session {session} is not a chat session"),
            ));
        };
        if !is_gateway_agent(&chat.agent) {
            return Err(error(
                ErrorCode::BadRequest,
                "attachments are only supported by direct and assistant chats",
            ));
        }
        let workspaces = self.workspaces().await?;
        let workspace = workspaces
            .iter()
            .find(|w| w.id == info.workspace_id)
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("workspace {} not found", info.workspace_id),
                )
            })?;
        let scope = Self::scope_of(workspace);
        let id = workspace.id.to_string();
        let (permissions, settings_key) = match scope {
            AssistantScope::Global => (self.settings.global_context, None),
            AssistantScope::Workspace => (
                self.settings
                    .permissions_for_workspace(&workspace.name, &id),
                [&id, &workspace.name]
                    .into_iter()
                    .find(|key| self.settings.workspaces.contains_key(*key))
                    .cloned(),
            ),
        };
        Ok(Access {
            scope,
            permissions,
            workspace_name: workspace.name.clone(),
            settings_key,
            roots: workspace.roots.clone(),
        })
    }

    /// `forbidden`, naming the setting that gates the attachment.
    fn forbidden(&self, access: &Access, kind: AttachmentKind) -> ErrorBody {
        let setting = gating_setting(kind);
        let (setting_path, whose) = match (&access.scope, &access.settings_key) {
            (AssistantScope::Global, _) => (
                format!("assistant.global_context.{setting}"),
                "the global assistant".to_owned(),
            ),
            (AssistantScope::Workspace, Some(key)) => (
                format!("assistant.workspaces.{key}.{setting}"),
                format!("workspace `{}`", access.workspace_name),
            ),
            (AssistantScope::Workspace, None) => (
                format!("assistant.context.{setting}"),
                format!("workspace `{}`", access.workspace_name),
            ),
        };
        let mut body = error(
            ErrorCode::Forbidden,
            format!(
                "`{}` attachments are not allowed for {whose}: `{setting_path}` is false in \
                 daemon.toml",
                kind.as_str()
            ),
        );
        body.data = Some(serde_json::json!({ "setting": setting_path }));
        body
    }

    async fn resolve(
        &self,
        access: &Access,
        attachment: &Attachment,
        share: usize,
    ) -> Result<Part, ErrorBody> {
        match attachment {
            Attachment::ShellBlock {
                terminal_id,
                block_id,
            } => self.shell_block(*terminal_id, *block_id, share),
            Attachment::ShellRecent { terminal_id, lines } => {
                self.shell_recent(*terminal_id, *lines, share)
            }
            Attachment::AgentTranscript {
                session_id,
                last_turns,
            } => self.transcript(session_id.trim(), *last_turns, share).await,
            Attachment::File { path, range } => {
                resolve_file(&access.roots, path, *range, share).await
            }
            Attachment::Search { query, limit } => self.search(query, *limit, share).await,
        }
    }

    fn shell_block(
        &self,
        terminal: TerminalId,
        block_id: u64,
        share: usize,
    ) -> Result<Part, ErrorBody> {
        let page = self.hub.blocks(TerminalBlocks {
            terminal_id: terminal,
            before_id: block_id.checked_add(1),
            limit: 1,
        })?;
        let block = page
            .blocks
            .into_iter()
            .find(|b| b.id == block_id)
            .ok_or_else(|| {
                error(
                    ErrorCode::NotFound,
                    format!("block {block_id} of terminal {terminal} not found"),
                )
            })?;
        let output = self.hub.block_text(TerminalBlockText {
            terminal_id: terminal,
            block_id,
            part: BlockPart::Output,
            max_bytes: u32::MAX,
        })?;
        let command = block.command.as_deref().map_or("", str::trim);
        let command = if command.is_empty() {
            "(no command)"
        } else {
            command
        };
        let status = match (block.exit_code, block.state) {
            (Some(code), _) => format!("exit {code}"),
            (None, BlockState::Running) => "running".to_owned(),
            (None, BlockState::Prompt) => "not run".to_owned(),
            (None, _) => "exit status unknown".to_owned(),
        };
        let description = format!(
            "{} · {status} · {} lines",
            one_line(command),
            output.text.lines().count()
        );
        let body = redact_and_bound(&output.text, share, Keep::End);
        Ok(Part::new(
            AttachmentKind::ShellBlock,
            command,
            &description,
            body,
            Keep::End,
            true,
            output.truncated,
        ))
    }

    fn shell_recent(
        &self,
        terminal: TerminalId,
        lines: u32,
        share: usize,
    ) -> Result<Part, ErrorBody> {
        let recent = self
            .hub
            .recent_text(terminal, usize::try_from(lines).unwrap_or(usize::MAX))?;
        let mut description = format!("terminal {terminal} · last {} lines", recent.lines);
        if recent.alternate_screen {
            description.push_str(" · full-screen program: visible screen only");
        }
        let body = redact_and_bound(&recent.text, share, Keep::End);
        Ok(Part::new(
            AttachmentKind::ShellRecent,
            &format!("terminal {terminal} · last {lines} lines"),
            &description,
            body,
            Keep::End,
            true,
            false,
        ))
    }

    async fn transcript(
        &self,
        session_id: &str,
        turns: u32,
        share: usize,
    ) -> Result<Part, ErrorBody> {
        let turns = usize::try_from(turns).unwrap_or(usize::MAX);
        if let Ok(id) = session_id.parse::<SessionId>()
            && let Some(session) = self
                .registry
                .list_sessions(SessionListFilter::default())
                .await?
                .sessions
                .into_iter()
                .find(|s| s.id == id && s.chat.is_some())
        {
            let messages = self.journal_messages(id).await?;
            return Ok(transcript_part(
                &session.title,
                "hutan chat",
                &messages,
                turns,
                share,
            ));
        }
        let rows = self
            .history
            .session_text(SessionTextQuery {
                key: session_id.to_owned(),
                sources: vec![Source::Claude, Source::Codex, Source::Omp],
                roles: vec![Role::User, Role::Assistant],
                limit: INDEX_ROWS,
            })
            .await?;
        let Some(first) = rows.first() else {
            return Err(error(
                ErrorCode::NotFound,
                format!(
                    "no conversation of session `{session_id}` is indexed (a Hutan chat id, or \
                     the id of a Claude Code, Codex or OMP session that `hutan search` finds)"
                ),
            ));
        };
        let source = first.source;
        let title = rows
            .iter()
            .find_map(|row| row.title.clone())
            .unwrap_or_else(|| session_id.to_owned());
        let messages = merge_chunks(rows.into_iter().map(|row| Message {
            user: row.role == Role::User,
            text: row.text,
        }));
        Ok(transcript_part(
            &title,
            source.as_str(),
            &messages,
            turns,
            share,
        ))
    }

    /// The user and assistant messages of a chat's journal, oldest first.
    async fn journal_messages(&self, id: SessionId) -> Result<Vec<Message>, ErrorBody> {
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || {
            storage
                .chat()
                .items_of_kind(id, "message_end", JOURNAL_ROWS)
        })
        .await
        .map_err(internal)?
        .map_err(|e| internal(format!("reading the journal: {e}")))?;
        Ok(rows
            .iter()
            .filter_map(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
            .filter_map(|event| match event.kind {
                AgentEventKind::MessageEnd { role, text, .. } if !text.trim().is_empty() => {
                    Some(Message {
                        user: role == MessageRole::User,
                        text,
                    })
                }
                _ => None,
            })
            .collect())
    }

    async fn search(&self, query: &str, limit: u32, share: usize) -> Result<Part, ErrorBody> {
        let hits = self
            .history
            .search(HistorySearch {
                query: query.to_owned(),
                limit,
                sources: Vec::new(),
                roles: Vec::new(),
                project: None,
                since_ms: None,
                until_ms: None,
            })
            .await?
            .hits;
        let lines: Vec<String> = hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let title = hit.title.as_deref().unwrap_or("untitled");
                let mut line = format!(
                    "{}. [{} {}] {} ({}",
                    index + 1,
                    hit.source.as_str(),
                    hit.session_key,
                    clip(title, MAX_TITLE_CHARS),
                    hit.role.as_str()
                );
                if let Some(cwd) = &hit.cwd {
                    line.push_str(&format!(", {cwd}"));
                }
                line.push_str(") — ");
                line.push_str(&clip(&hit.snippet.replace(['«', '»'], ""), SNIPPET_CHARS));
                line
            })
            .collect();
        let body = redact_and_bound(&lines.join("\n"), share, Keep::Start);
        Ok(Part::new(
            AttachmentKind::Search,
            &format!("\"{query}\""),
            &format!("\"{}\" · {} hits", one_line(query), hits.len()),
            body,
            Keep::Start,
            true,
            false,
        ))
    }
}

/// Longest session title in a search line, in characters.
const MAX_TITLE_CHARS: usize = 60;

/// `text` on one line, at most `max` characters, a cut ending in `…`.
fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = context::bound(&line, max, Keep::Start);
    if cut.truncated {
        format!("{}…", cut.text)
    } else {
        cut.text
    }
}

/// The part for the last `turns` turns of a conversation.
fn transcript_part(
    title: &str,
    origin: &str,
    messages: &[Message],
    turns: usize,
    share: usize,
) -> Part {
    let (text, count) = last_turns(messages, turns);
    let body = redact_and_bound(&text, share, Keep::End);
    Part::new(
        AttachmentKind::AgentTranscript,
        title,
        &format!("{} · {origin} · {count} turns", one_line(title)),
        body,
        Keep::End,
        true,
        false,
    )
}

/// Reads a file attachment below `roots` (a relative `path` is relative to the first root).
/// Blocking file system work runs on the blocking pool; the file is opened without following
/// symlinks below the root ([`read_file`]).
pub(crate) async fn resolve_file(
    roots: &[PathBuf],
    path: &str,
    range: Option<LineRange>,
    share: usize,
) -> Result<Part, ErrorBody> {
    let roots = roots.to_vec();
    let requested = PathBuf::from(path);
    let max_chars = share.saturating_add(REDACT_SLACK);
    let read = tokio::task::spawn_blocking(move || -> io::Result<(FileContext, String)> {
        // A root that is gone cannot hold the file and must not fail the others.
        let roots: Vec<PathBuf> = roots.iter().filter_map(|r| r.canonicalize().ok()).collect();
        let Some(first) = roots.first() else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the workspace has no readable folder",
            ));
        };
        let target = if requested.is_absolute() {
            requested
        } else {
            first.join(requested)
        };
        let context = read_file(&target, &roots, range.map(|r| (r.start, r.end)), max_chars)?;
        let shown = roots
            .iter()
            .find_map(|root| context.path.strip_prefix(root).ok())
            .map_or_else(
                || context.path.display().to_string(),
                |relative| relative.display().to_string(),
            );
        Ok((context, shown))
    })
    .await
    .map_err(internal)?;
    let (context, shown) = read.map_err(|e| file_error(path, &e))?;
    let range_note = range.map_or_else(String::new, |r| format!(" · lines {}-{}", r.start, r.end));
    let label = range.map_or_else(
        || shown.clone(),
        |r| format!("{shown}:{}-{}", r.start, r.end),
    );
    let body: Bounded = redact_and_bound(&context.text, share, Keep::Start);
    let description = format!(
        "{}{range_note} · {} lines",
        one_line(&shown),
        context.text.lines().count()
    );
    Ok(Part::new(
        AttachmentKind::File,
        &label,
        &description,
        body,
        Keep::Start,
        !context.truncated,
        context.truncated,
    ))
}

fn file_error(path: &str, error_: &io::Error) -> ErrorBody {
    let shown = one_line(path);
    match error_.kind() {
        io::ErrorKind::PermissionDenied => error(
            ErrorCode::Forbidden,
            format!(
                "`{shown}` is outside the folders of this workspace: files are only read below \
                 its roots"
            ),
        ),
        io::ErrorKind::NotFound => error(ErrorCode::NotFound, format!("file `{shown}` not found")),
        io::ErrorKind::InvalidInput => error(
            ErrorCode::BadRequest,
            format!("`{shown}` cannot be attached: {error_}"),
        ),
        io::ErrorKind::InvalidData => error(
            ErrorCode::BadRequest,
            format!("`{shown}` is not valid UTF-8 text"),
        ),
        _ => match error_.raw_os_error() {
            // A symlink replaced a path component after the path was checked.
            Some(code) if code == libc::ELOOP || code == libc::ENOTDIR => error(
                ErrorCode::Forbidden,
                format!("`{shown}` goes through a symbolic link, which is not followed"),
            ),
            _ => internal(format!("reading `{shown}`: {error_}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn roots(dir: &tempfile::TempDir) -> Vec<PathBuf> {
        vec![dir.path().to_path_buf()]
    }

    fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    async fn code(result: impl Future<Output = Result<Part, ErrorBody>>) -> ErrorCode {
        result.await.expect_err("refused").code
    }

    #[tokio::test]
    async fn a_file_is_headed_redacted_and_cut_to_its_share() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        write(
            &dir,
            "src/main.rs",
            &format!("fn main() {{}}\nlet key = \"{secret}\";\nlet more = 1;\n"),
        );
        let part = resolve_file(&roots(&dir), "src/main.rs", None, 1000)
            .await
            .unwrap();
        assert_eq!(part.header, "[file: src/main.rs · 3 lines]");
        assert!(part.body.contains("fn main()"));
        assert!(!part.body.contains("sk-abc"), "{}", part.body);
        assert!(!part.truncated);

        // Half of the share: a cut, noted in the header, with a known total.
        let cut = resolve_file(&roots(&dir), "src/main.rs", None, 20)
            .await
            .unwrap();
        assert!(cut.truncated);
        assert_eq!(cut.body.chars().count(), 20);
        assert!(
            cut.header
                .starts_with("[file: src/main.rs · 3 lines · cut to the first 20 of "),
            "{}",
            cut.header
        );
        assert_eq!(cut.meta().chars, 20);
    }

    #[tokio::test]
    async fn a_range_reads_only_those_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "notes.txt", "one\ntwo\nthree\nfour\n");
        let part = resolve_file(
            &roots(&dir),
            "notes.txt",
            Some(LineRange { start: 2, end: 3 }),
            100,
        )
        .await
        .unwrap();
        assert_eq!(part.body, "two\nthree");
        assert_eq!(part.header, "[file: notes.txt · lines 2-3 · 2 lines]");
        assert_eq!(part.label, "notes.txt:2-3");
    }

    #[tokio::test]
    async fn files_outside_the_roots_are_forbidden_whatever_the_route() {
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = write(&outside, "secret.txt", "top secret");
        write(&work, "inside.txt", "fine");

        // An absolute path elsewhere.
        assert_eq!(
            code(resolve_file(
                &roots(&work),
                secret.to_str().unwrap(),
                None,
                100
            ))
            .await,
            ErrorCode::Forbidden
        );
        // `..` out of the root, relative and absolute.
        let relative = format!(
            "../{}/secret.txt",
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(
            code(resolve_file(&roots(&work), &relative, None, 100)).await,
            ErrorCode::Forbidden
        );
        let dotted = format!(
            "{}/../{}",
            work.path().display(),
            relative.trim_start_matches("../")
        );
        assert_eq!(
            code(resolve_file(&roots(&work), &dotted, None, 100)).await,
            ErrorCode::Forbidden
        );
        // A symlink inside the root that points outside, to a file and to a directory.
        symlink(&secret, work.path().join("link.txt")).unwrap();
        symlink(outside.path(), work.path().join("linkdir")).unwrap();
        assert_eq!(
            code(resolve_file(&roots(&work), "link.txt", None, 100)).await,
            ErrorCode::Forbidden
        );
        assert_eq!(
            code(resolve_file(&roots(&work), "linkdir/secret.txt", None, 100)).await,
            ErrorCode::Forbidden
        );
        // A sibling directory that merely shares the root's name as a prefix.
        let sibling = PathBuf::from(format!("{}-evil", work.path().display()));
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("x.txt"), "x").unwrap();
        assert_eq!(
            code(resolve_file(
                &roots(&work),
                sibling.join("x.txt").to_str().unwrap(),
                None,
                100
            ))
            .await,
            ErrorCode::Forbidden
        );
        std::fs::remove_dir_all(&sibling).unwrap();

        // Inside is fine, also through a symlink that stays inside.
        symlink(
            work.path().join("inside.txt"),
            work.path().join("alias.txt"),
        )
        .unwrap();
        let alias = resolve_file(&roots(&work), "alias.txt", None, 100)
            .await
            .unwrap();
        assert_eq!(alias.body, "fine");
        assert!(
            alias.header.starts_with("[file: inside.txt"),
            "{}",
            alias.header
        );
        assert_eq!(
            code(resolve_file(&roots(&work), "missing.txt", None, 100)).await,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn a_workspace_without_a_readable_folder_reads_nothing() {
        let gone = PathBuf::from("/definitely/not/a/folder");
        assert_eq!(
            code(resolve_file(&[gone], "x", None, 100)).await,
            ErrorCode::Forbidden
        );
        assert_eq!(
            code(resolve_file(&[], "x", None, 100)).await,
            ErrorCode::Forbidden
        );
    }

    #[tokio::test]
    async fn a_missing_root_does_not_hide_the_others() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "a.txt", "alpha");
        let roots = vec![PathBuf::from("/definitely/not/a/folder"), dir.path().into()];
        let part = resolve_file(&roots, "a.txt", None, 100).await.unwrap();
        // A relative path is relative to the first root that exists.
        assert_eq!(part.body, "alpha");
    }

    #[tokio::test]
    async fn binary_and_directory_attachments_are_bad_requests() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("blob.bin");
        std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(
            code(resolve_file(&roots(&dir), "blob.bin", None, 100)).await,
            ErrorCode::BadRequest
        );
        assert_eq!(
            code(resolve_file(&roots(&dir), "sub", None, 100)).await,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn a_transcript_names_its_origin_and_keeps_the_newest_turns() {
        let messages = [
            Message {
                user: true,
                text: "old question".into(),
            },
            Message {
                user: false,
                text: "old answer".into(),
            },
            Message {
                user: true,
                text: "new question".into(),
            },
            Message {
                user: false,
                text: "new answer".into(),
            },
        ];
        let part = transcript_part("Scheduler stall", "claude", &messages, 1, 1000);
        assert_eq!(part.header, "[agent: Scheduler stall · claude · 1 turns]");
        assert_eq!(part.body, "user: new question\n\nassistant: new answer");
        // A share that is too small keeps the end (the newest words) and says so.
        let tight = transcript_part("Scheduler stall", "claude", &messages, 2, 20);
        assert!(tight.truncated);
        assert!(tight.body.ends_with("new answer"), "{}", tight.body);
        assert!(
            tight.header.contains("cut to the last 20 of"),
            "{}",
            tight.header
        );
    }
}
