//! `hutan wt new|ls|rm` and the worktree step of `hutan new --worktree`.

use std::path::{Path, PathBuf};

use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};
use hutan_protocol::worktree::{
    WorktreeCreate, WorktreeCreated, WorktreeInfo, WorktreeList, WorktreeRemove,
};
use serde_json::json;

use super::Ctx;
use super::sessions::with_source;
use super::workspaces::workspace_target;
use crate::args::{WtCommand, WtLsArgs, WtNewArgs, WtRmArgs};
use crate::error::CliError;

/// Where `hutan new --worktree` starts its session.
pub(super) struct Placed {
    pub workspace: WorkspaceId,
    pub path: PathBuf,
}

impl Ctx {
    pub(super) async fn wt(&self, command: WtCommand) -> Result<(), CliError> {
        match command {
            WtCommand::New(args) => self.wt_new(args).await,
            WtCommand::Ls(args) => self.wt_ls(args).await,
            WtCommand::Rm(args) => self.wt_rm(args).await,
        }
    }

    async fn wt_new(&self, args: WtNewArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let (workspace, from_cwd) = self
            .wt_scope(&client, args.workspace)
            .await?
            .ok_or_else(no_workspace)?;
        // The daemon resolves paths from its own working directory, not ours.
        let repo = match args.repo {
            Some(path) => Some(std::path::absolute(path)?),
            None if from_cwd => Some(std::env::current_dir()?),
            None => None,
        };
        let path = args.path.map(std::path::absolute).transpose()?;
        let created = client
            .worktree_create(WorktreeCreate {
                workspace_id: workspace,
                repo,
                branch: args.branch,
                base: args.base,
                path,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let path = created.path.display().to_string();
        self.print(&WorktreeCreated { worktree: created }, || path)
    }

    async fn wt_ls(&self, args: WtLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let worktrees = client
            .worktree_list(workspace)
            .await
            .map_err(|e| self.fail(e))?;
        let list = WorktreeList { worktrees };
        self.print(&list, || worktree_table(&list.worktrees))
    }

    async fn wt_rm(&self, args: WtRmArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let list = client
            .worktree_list(workspace)
            .await
            .map_err(|e| self.fail(e))?;
        let target = resolve_worktree(&list, &args.target, &std::env::current_dir()?)?.clone();
        let removed = client
            .worktree_remove(WorktreeRemove {
                path: target.path.clone(),
                force: args.force,
                delete_branch: args.delete_branch,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(
            &json!({
                "path": target.path,
                "removed": true,
                "branch_kept": removed.branch_kept,
            }),
            || {
                let mut text = format!("removed {}", target.path.display());
                if let Some(reason) = &removed.branch_kept {
                    text.push_str(&format!(
                        "\nbranch {} kept: {reason}",
                        target.branch.as_deref().unwrap_or("-")
                    ));
                }
                text
            },
        )
    }

    /// The directory and workspace of `hutan new --worktree <branch>`: the branch's existing
    /// worktree, or a new one at the default path.
    pub(super) async fn worktree_for_session(
        &self,
        client: &Client,
        workspace_flag: Option<String>,
        branch: &str,
        cwd: &Path,
    ) -> Result<Placed, CliError> {
        let (workspace, from_cwd) = self
            .wt_scope(client, workspace_flag)
            .await?
            .ok_or_else(no_workspace)?;
        let hint = from_cwd.then_some(cwd);
        let list = client
            .worktree_list(Some(workspace))
            .await
            .map_err(|e| self.fail(e))?;
        if let Some(existing) = reusable(&list, branch, hint)? {
            self.note(format!("using worktree {}", existing.path.display()));
            return Ok(Placed {
                workspace,
                path: existing.path.clone(),
            });
        }
        let created = client
            .worktree_create(WorktreeCreate {
                workspace_id: workspace,
                repo: hint.map(Path::to_path_buf),
                branch: branch.to_owned(),
                base: None,
                path: None,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.note(format!("created worktree {}", created.path.display()));
        Ok(Placed {
            workspace,
            path: created.path,
        })
    }

    /// A line for people on stderr; nothing in `--json` mode.
    fn note(&self, text: String) {
        if !self.json {
            eprintln!("{text}");
        }
    }

    /// The workspace the worktree commands act on and whether the current directory chose it:
    /// `--ws`, else `$HUTAN_WORKSPACE`, else the workspace containing the current directory.
    pub(super) async fn wt_scope(
        &self,
        client: &Client,
        flag: Option<String>,
    ) -> Result<Option<(WorkspaceId, bool)>, CliError> {
        if let Some((target, source)) = workspace_target(flag) {
            let id = self
                .resolve_workspace_id(client, &target)
                .await
                .map_err(|error| with_source(error, &target, source))?;
            return Ok(Some((id, false)));
        }
        let all = self.workspace_list(client).await?.workspaces;
        let cwd = std::env::current_dir()?;
        Ok(workspace_containing(&all, &cwd).map(|w| (w.id, true)))
    }
}

pub(super) fn no_workspace() -> CliError {
    CliError::Usage(
        "no workspace contains the current directory; pass --ws, set $HUTAN_WORKSPACE, or run \
         `hutan ws open .` first"
            .to_owned(),
    )
}

/// The project workspace with the deepest root containing `dir` (Home never counts: it is not
/// a repository).
fn workspace_containing<'a>(
    workspaces: &'a [WorkspaceInfo],
    dir: &Path,
) -> Option<&'a WorkspaceInfo> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    workspaces
        .iter()
        .filter(|w| w.kind == WorkspaceKind::Project)
        .filter_map(|w| {
            w.roots
                .iter()
                .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
                .filter(|root| dir.starts_with(root))
                .map(|root| root.components().count())
                .max()
                .map(|depth| (depth, w))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, w)| w)
}

/// The worktree `target` names: a path (as written, or relative to `cwd`), else a branch.
fn resolve_worktree<'a>(
    list: &'a [WorktreeInfo],
    target: &str,
    cwd: &Path,
) -> Result<&'a WorktreeInfo, CliError> {
    let written = cwd.join(target);
    let canonical = std::fs::canonicalize(&written).ok();
    if let Some(found) = list
        .iter()
        .find(|w| w.path == written || canonical.as_deref() == Some(w.path.as_path()))
    {
        return Ok(found);
    }
    let by_branch: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| w.branch.as_deref() == Some(target))
        .collect();
    match by_branch.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no worktree matches `{target}`; `hutan wt ls` lists them"
        ))),
        many => {
            let mut message = format!(
                "`{target}` is checked out in {} worktrees; use a path:",
                many.len()
            );
            for w in many {
                message.push_str(&format!("\n  {}", w.path.display()));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// The existing worktree `hutan new --worktree <branch>` should use, if any. `hint` is a
/// directory of the repository the user is in; without it every repository of the workspace
/// counts.
fn reusable<'a>(
    list: &'a [WorktreeInfo],
    branch: &str,
    hint: Option<&Path>,
) -> Result<Option<&'a WorktreeInfo>, CliError> {
    let hint = hint.map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
    // The repository the directory is in: the deepest checkout containing it.
    let repo = hint.as_deref().and_then(|dir| {
        list.iter()
            .filter(|w| dir.starts_with(&w.path))
            .max_by_key(|w| w.path.components().count())
            .map(|w| w.repo_root.as_path())
    });
    let matching: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| w.branch.as_deref() == Some(branch))
        .filter(|w| repo.is_none_or(|repo| w.repo_root == repo))
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] if one.main => Err(CliError::Failed(format!(
            "branch `{branch}` is checked out in the main checkout {}; a worktree needs a branch \
             that is not (start the session there with `hutan new --cwd`)",
            one.path.display()
        ))),
        [one] => Ok(Some(one)),
        many => {
            let mut message = format!(
                "branch `{branch}` has worktrees in {} repositories; run from inside one:",
                many.len()
            );
            for w in many {
                message.push_str(&format!("\n  {}", w.path.display()));
            }
            Err(CliError::Usage(message))
        }
    }
}

fn branch_text(worktree: &WorktreeInfo) -> String {
    match &worktree.branch {
        Some(branch) => branch.clone(),
        None => format!(
            "(detached {})",
            worktree.head.chars().take(7).collect::<String>()
        ),
    }
}

fn state_text(worktree: &WorktreeInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if worktree.main {
        parts.push("main".to_owned());
    }
    if worktree.created_by_hutan {
        parts.push("hutan".to_owned());
    }
    if worktree.dirty {
        parts.push("dirty".to_owned());
    }
    if worktree.locked {
        parts.push("locked".to_owned());
    }
    if worktree.prunable {
        parts.push("prunable".to_owned());
    }
    match worktree.live_sessions.len() {
        0 => {}
        1 => parts.push("1 session".to_owned()),
        n => parts.push(format!("{n} sessions")),
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(", ")
    }
}

fn worktree_table(worktrees: &[WorktreeInfo]) -> String {
    if worktrees.is_empty() {
        return "no worktrees".to_owned();
    }
    let rows: Vec<[String; 3]> = worktrees
        .iter()
        .map(|w| [branch_text(w), w.path.display().to_string(), state_text(w)])
        .collect();
    let header = ["BRANCH", "PATH", "STATE"].map(str::to_owned);
    let mut widths = [0; 3];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            format!(
                "{:<w0$}  {:<w1$}  {}",
                row[0],
                row[1],
                row[2],
                w0 = widths[0],
                w1 = widths[1]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, repo: &str, branch: Option<&str>, main: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            repo_root: PathBuf::from(repo),
            branch: branch.map(str::to_owned),
            head: "3f786850e387550fdab836ed7e6dc881de23001b".to_owned(),
            main,
            created_by_hutan: false,
            dirty: false,
            live_sessions: vec![],
            locked: false,
            prunable: false,
        }
    }

    fn fixture() -> Vec<WorktreeInfo> {
        vec![
            wt("/w/a", "/w/a", Some("main"), true),
            wt("/w/a-wt/x", "/w/a", Some("feature/x"), false),
            wt("/w/b", "/w/b", Some("main"), true),
            wt("/w/b-wt/x", "/w/b", Some("feature/x"), false),
            wt("/w/b-wt/bisect", "/w/b", None, false),
        ]
    }

    #[test]
    fn a_target_is_a_path_first_then_a_branch() {
        let list = fixture();
        let cwd = Path::new("/w/b");
        assert_eq!(
            resolve_worktree(&list, "/w/a-wt/x", cwd).unwrap().path,
            Path::new("/w/a-wt/x")
        );
        assert!(
            resolve_worktree(&list, "../b-wt/x", cwd).is_err(),
            "relative paths are joined, not normalised: no such entry"
        );
        assert_eq!(
            resolve_worktree(&list, "b-wt/bisect", Path::new("/w"))
                .unwrap()
                .path,
            Path::new("/w/b-wt/bisect")
        );
        // A branch in one repository only.
        let list = &list[..3];
        assert_eq!(
            resolve_worktree(list, "feature/x", cwd).unwrap().path,
            Path::new("/w/a-wt/x")
        );
    }

    #[test]
    fn an_ambiguous_branch_is_a_usage_error_and_an_unknown_one_a_failure() {
        let list = fixture();
        let error = resolve_worktree(&list, "feature/x", Path::new("/")).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        assert!(error.to_string().contains("/w/a-wt/x") && error.to_string().contains("/w/b-wt/x"));
        let error = resolve_worktree(&list, "nope", Path::new("/")).unwrap_err();
        assert!(matches!(error, CliError::Failed(_)));
    }

    #[test]
    fn an_existing_worktree_is_reused_and_the_main_checkout_is_refused() {
        let list = fixture();
        assert!(reusable(&list, "unknown", None).unwrap().is_none());
        // Two repositories have the branch: the directory decides.
        assert!(matches!(
            reusable(&list, "feature/x", None),
            Err(CliError::Usage(_))
        ));
        assert_eq!(
            reusable(&list, "feature/x", Some(Path::new("/w/b/src")))
                .unwrap()
                .unwrap()
                .path,
            Path::new("/w/b-wt/x")
        );
        // From inside a worktree the repository is the same.
        assert_eq!(
            reusable(&list, "feature/x", Some(Path::new("/w/a-wt/x/deep")))
                .unwrap()
                .unwrap()
                .path,
            Path::new("/w/a-wt/x")
        );
        let error = reusable(&list, "main", Some(Path::new("/w/a"))).unwrap_err();
        assert!(matches!(error, CliError::Failed(_)), "{error:?}");
        assert!(error.to_string().contains("main checkout"));
    }

    #[test]
    fn the_table_shows_branch_path_and_state() {
        let mut list = fixture();
        list[1].created_by_hutan = true;
        list[1].dirty = true;
        list[1].live_sessions = vec!["01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap()];
        list[4].locked = true;
        list[4].prunable = true;
        let table = worktree_table(&list);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("BRANCH") && lines[0].ends_with("STATE"));
        assert!(
            lines[1].contains("main") && lines[1].ends_with("main"),
            "{table}"
        );
        assert!(lines[2].ends_with("hutan, dirty, 1 session"), "{table}");
        assert!(
            lines[5].starts_with("(detached 3f78685)") && lines[5].ends_with("locked, prunable"),
            "{table}"
        );
        assert_eq!(worktree_table(&[]), "no worktrees");
    }

    #[test]
    fn the_workspace_is_found_by_the_deepest_root_containing_the_directory() {
        use hutan_protocol::messages::CreatedBy;
        let ws = |name: &str, roots: &[&str], kind| WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.to_owned(),
            roots: roots.iter().map(PathBuf::from).collect(),
            kind,
            pinned: false,
            position: 0,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
        };
        let all = [
            ws("home", &["/w"], WorkspaceKind::Home),
            ws("outer", &["/w/p"], WorkspaceKind::Project),
            ws("inner", &["/w/p/sub"], WorkspaceKind::Project),
            ws("other", &["/x"], WorkspaceKind::Project),
        ];
        let name = |dir: &str| workspace_containing(&all, Path::new(dir)).map(|w| w.name.as_str());
        assert_eq!(name("/w/p/src"), Some("outer"));
        assert_eq!(name("/w/p/sub/deep"), Some("inner"));
        assert_eq!(name("/w/other"), None, "Home never counts");
        assert_eq!(name("/y"), None);
    }
}
