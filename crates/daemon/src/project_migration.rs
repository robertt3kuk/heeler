//! The one-time conversion of the workspaces of earlier versions (protocol 0.19, ADR 0014).
//!
//! Every workspace stays a workspace — its id, name, pin, order, layout, tasks and sessions are
//! untouched — and every folder it had becomes a **member project**. Which project that is
//! needs the filesystem (a project's identity is the git common directory of its repository),
//! so this cannot be a SQL migration: the daemon runs it at its first start on schema 8, before
//! the registry loads anything, and notes it in `app_meta` ([`MARKER`]). Everything it changes is
//! written in one transaction together with the marker.
//!
//! - A root becomes the project of its repository, named after the repository's main checkout
//!   (a subfolder or a linked worktree resolves to it); a folder outside git is its own project;
//!   a folder that no longer exists keeps a folder project under the path it was stored with.
//! - A project is in at most one workspace: when two workspaces had folders of the same
//!   repository, the first (in list order) keeps the project.
//! - Member projects follow their workspace in the list; positions are renumbered `0..n`. Home
//!   stays where it is, and no session moves: new sessions find their project by their folder.

use hutan_core::WorkspaceId;
use hutan_storage::{Storage, StorageError, WorkspaceKind, WorkspaceRow, Write};
use tracing::{info, warn};

use crate::projects::{self, Ident};
use crate::restore::Stored;

/// `app_meta` key set once the conversion is done.
pub const MARKER: &str = "projects_auto";

/// Converts the stored workspaces unless that was done before, in `stored` and in the database.
/// Blocking: run it off the async threads.
pub fn run(storage: &Storage, stored: &mut Stored) -> Result<(), StorageError> {
    if storage.app_meta().get(MARKER)?.is_some() {
        return Ok(());
    }
    let converted = plan(&stored.workspaces);
    let mut writes = Vec::new();
    for row in &converted {
        match stored.workspaces.iter().find(|old| old.id == row.id) {
            None => writes.push(Write::InsertWorkspace(row.clone())),
            Some(old) => {
                if old.kind != row.kind {
                    writes.push(Write::SetWorkspaceKind {
                        id: row.id,
                        kind: row.kind,
                    });
                }
                if old.roots != row.roots {
                    writes.push(Write::SetWorkspaceRoots {
                        id: row.id,
                        roots: row.roots.clone(),
                    });
                }
                if old.position != row.position {
                    writes.push(Write::SetWorkspacePosition {
                        id: row.id,
                        position: row.position,
                    });
                }
            }
        }
    }
    writes.push(Write::SetMeta {
        key: MARKER.to_owned(),
        value: "1".to_owned(),
    });
    storage.batch(writes)?;
    storage.flush()?;
    let made = converted.len() - stored.workspaces.len();
    if made > 0 {
        info!(
            projects = made,
            "workspaces converted: their folders are projects now"
        );
    }
    stored.workspaces = converted;
    Ok(())
}

/// The workspace rows after the conversion, in list order, positions `0..n`. Touches nothing.
pub fn plan(workspaces: &[WorkspaceRow]) -> Vec<WorkspaceRow> {
    let mut rows: Vec<WorkspaceRow> = Vec::with_capacity(workspaces.len());
    for workspace in workspaces {
        if workspace.kind != WorkspaceKind::Project {
            rows.push(workspace.clone());
            continue;
        }
        let container = WorkspaceRow {
            kind: WorkspaceKind::Workspace,
            roots: Vec::new(),
            common_dir: None,
            parent_id: None,
            ..workspace.clone()
        };
        let id = container.id;
        rows.push(container);
        for root in &workspace.roots {
            let ident = identity_of(root);
            match rows.iter().position(|row| is_project_of(row, &ident)) {
                Some(at) if rows[at].parent_id == Some(id) => {}
                Some(_) => warn!(
                    workspace = %id,
                    folder = %ident.root.display(),
                    "the project is already in another workspace; this one does not get it"
                ),
                None => rows.push(WorkspaceRow {
                    id: WorkspaceId::new(),
                    name: ident.name(),
                    roots: vec![ident.root.clone()],
                    kind: WorkspaceKind::Project,
                    pinned: false,
                    created_by: workspace.created_by,
                    created_at_ms: workspace.created_at_ms,
                    last_active_ms: workspace.last_active_ms,
                    position: 0,
                    common_dir: ident.common_dir,
                    parent_id: Some(id),
                }),
            }
        }
    }
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = i64::try_from(position).unwrap_or(i64::MAX);
    }
    rows
}

/// The project a stored root stands for: its repository's, else the folder; a folder that is
/// gone stays as it was stored.
fn identity_of(root: &std::path::Path) -> Ident {
    std::fs::canonicalize(root)
        .ok()
        .filter(|dir| dir.is_dir())
        .map(|dir| {
            projects::identify(&dir).unwrap_or(Ident {
                common_dir: None,
                root: dir,
            })
        })
        .unwrap_or_else(|| Ident {
            common_dir: None,
            root: root.to_path_buf(),
        })
}

/// `row` is the project `ident` names: the same repository, or (a folder) the same folder.
fn is_project_of(row: &WorkspaceRow, ident: &Ident) -> bool {
    row.kind == WorkspaceKind::Project
        && match &ident.common_dir {
            Some(wanted) => row.common_dir.as_ref() == Some(wanted),
            None => row.common_dir.is_none() && row.roots.first() == Some(&ident.root),
        }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use hutan_storage::CreatedBy;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
        root
    }

    fn base() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        (dir, base)
    }

    fn workspace(name: &str, roots: &[&Path], position: i64) -> WorkspaceRow {
        WorkspaceRow {
            id: WorkspaceId::new(),
            name: name.to_owned(),
            roots: roots.iter().map(|r| r.to_path_buf()).collect(),
            kind: WorkspaceKind::Project,
            pinned: false,
            created_by: CreatedBy::Cli,
            created_at_ms: 1_000,
            last_active_ms: 2_000,
            position,
            common_dir: None,
            parent_id: None,
        }
    }

    fn home(position: i64) -> WorkspaceRow {
        WorkspaceRow {
            kind: WorkspaceKind::Home,
            ..workspace("Home", &[Path::new("/home-folder")], position)
        }
    }

    fn names(rows: &[WorkspaceRow]) -> Vec<&str> {
        rows.iter().map(|w| w.name.as_str()).collect()
    }

    #[test]
    fn a_workspace_with_one_root_stays_and_gets_its_repository_as_a_project() {
        let (_dir, base) = base();
        let main = repo(&base, "app");
        fs::create_dir_all(main.join("crates/x")).unwrap();
        let mut pinned = workspace("mine", &[&main.join("crates/x")], 5);
        pinned.pinned = true;

        let home = home(-1);
        let rows = plan(&[home.clone(), pinned.clone()]);
        assert_eq!(names(&rows), ["Home", "mine", "app"]);
        // Home is untouched, the workspace keeps id, name, pin, creator and place in the list.
        assert_eq!(
            rows[0],
            WorkspaceRow {
                position: 0,
                ..home
            }
        );
        let container = &rows[1];
        assert_eq!(container.id, pinned.id);
        assert_eq!(container.kind, WorkspaceKind::Workspace);
        assert!(container.pinned && container.roots.is_empty());
        assert_eq!(container.created_by, CreatedBy::Cli);
        assert_eq!(container.position, 1);
        // A subfolder resolves to its repository's main checkout; the project is a member.
        let project = &rows[2];
        assert_eq!(project.kind, WorkspaceKind::Project);
        assert_eq!(project.roots, std::slice::from_ref(&main));
        assert_eq!(
            project.common_dir.as_deref(),
            Some(fs::canonicalize(main.join(".git")).unwrap().as_path())
        );
        assert_eq!(project.parent_id, Some(pinned.id));
        assert!(!project.pinned);
        assert_eq!((project.position, project.created_by), (2, CreatedBy::Cli));
    }

    #[test]
    fn several_roots_become_member_projects_in_order() {
        let (_dir, base) = base();
        let api = repo(&base, "api");
        let web = repo(&base, "web");
        let notes = base.join("notes");
        fs::create_dir_all(&notes).unwrap();
        let api_wt = base.join("api-wt");
        git(
            &api,
            &["worktree", "add", "-q", "-b", "f", api_wt.to_str().unwrap()],
        );
        let gone = base.join("gone");
        let shop = workspace("shop", &[&web, &notes, &api_wt, &api, &gone, &notes], 0);
        let empty = workspace("empty", &[], 1);

        let rows = plan(&[shop.clone(), empty.clone()]);
        // The worktree and the main checkout are one project, a repeated folder adds nothing.
        assert_eq!(
            names(&rows),
            ["shop", "web", "notes", "api", "gone", "empty"]
        );
        let member = |name: &str| rows.iter().find(|w| w.name == name).unwrap();
        assert_eq!(
            member("api").roots,
            std::slice::from_ref(&api),
            "the main checkout, not the worktree"
        );
        assert_eq!(member("notes").common_dir, None);
        assert_eq!(
            member("gone").roots,
            [gone],
            "a missing folder keeps its stored path"
        );
        for name in ["web", "notes", "api", "gone"] {
            assert_eq!(member(name).parent_id, Some(shop.id), "{name}");
        }
        assert_eq!(member("empty").kind, WorkspaceKind::Workspace);
        assert!(rows.iter().all(|w| w.parent_id != Some(empty.id)));
        let positions: Vec<_> = rows.iter().map(|w| w.position).collect();
        assert_eq!(positions, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_repository_in_two_workspaces_stays_in_the_first() {
        let (_dir, base) = base();
        let shared = repo(&base, "shared");
        let only = repo(&base, "only");
        let first = workspace("first", &[&shared], 0);
        let second = workspace("second", &[&only, &shared], 1);

        let rows = plan(&[first.clone(), second.clone()]);
        assert_eq!(names(&rows), ["first", "shared", "second", "only"]);
        let shared_row = rows.iter().find(|w| w.name == "shared").unwrap();
        assert_eq!(shared_row.parent_id, Some(first.id));
        let only_row = rows.iter().find(|w| w.name == "only").unwrap();
        assert_eq!(only_row.parent_id, Some(second.id));
    }

    #[test]
    fn the_conversion_is_stored_once() {
        let (dir, base) = base();
        let app = repo(&base, "app");
        let storage = Storage::open(dir.path().join("hutan.db")).unwrap();
        let old = workspace("old", &[&app], 1);
        storage.workspaces().insert(home(0)).unwrap();
        storage.workspaces().insert(old.clone()).unwrap();
        let rows = storage.workspaces().list().unwrap();
        let mut stored = Stored {
            workspaces: rows,
            ..Stored::default()
        };

        run(&storage, &mut stored).unwrap();
        assert_eq!(names(&stored.workspaces), ["Home", "old", "app"]);
        let reread = storage.workspaces().list().unwrap();
        assert_eq!(reread, stored.workspaces);
        assert_eq!(reread[1].kind, WorkspaceKind::Workspace);
        assert!(reread[1].roots.is_empty());
        assert_eq!(reread[2].parent_id, Some(old.id));
        assert!(reread[2].common_dir.is_some());

        // A second start does nothing, even though the list now holds a project.
        let mut again = Stored {
            workspaces: reread.clone(),
            ..Stored::default()
        };
        run(&storage, &mut again).unwrap();
        assert_eq!(again.workspaces, reread);
        assert_eq!(storage.workspaces().list().unwrap(), reread);
    }
}
