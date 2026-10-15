//! Session placement (protocol 0.14): which git checkout a session works in, and which other
//! live agents work in the same one (`SessionInfo.placement`, `docs/orchestration.md`).
//!
//! Everything here reads the filesystem, never runs git: the checkout containing a directory is
//! found by walking up to the nearest `.git` entry, a linked worktree is told from a main
//! checkout by the `commondir` file git keeps in its private directory, and the branch is read
//! from `HEAD`. The directory part is cached ([`PlacementCache`]) because a shell reports its
//! directory at every prompt; the (tiny) `HEAD` read is repeated on every derivation so a branch
//! switch shows up at the next one.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hutan_core::SessionId;
use hutan_protocol::worktree::{Placement, PlacementKind};

use crate::projects;

/// Directories remembered before the cache starts over.
const CACHE_LIMIT: usize = 512;

/// A cached answer is trusted this long (a `git init` or a moved checkout is noticed at the
/// latest then, without any request invalidating the cache).
const CACHE_TTL: Duration = Duration::from_secs(60);

/// A git checkout found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub kind: PlacementKind,
    /// The top level of the checkout (canonical).
    pub root: PathBuf,
    /// The file holding the checked-out ref (`HEAD` of the checkout's private git directory).
    head: PathBuf,
}

impl Checkout {
    /// The checked-out branch; `None` for a detached HEAD or an unreadable `HEAD`.
    pub fn branch(&self) -> Option<String> {
        fs::read_to_string(&self.head)
            .ok()
            .and_then(|text| branch_from_head(&text))
    }

    /// The placement of a session in this checkout, with nobody sharing it yet.
    pub fn placement(&self) -> Placement {
        Placement {
            kind: self.kind,
            root: self.root.clone(),
            branch: self.branch(),
            shared_with: Vec::new(),
        }
    }
}

/// The branch a `HEAD` file names: `ref: refs/heads/<branch>`; anything else (a commit id, a
/// ref outside `refs/heads`) is no branch.
pub fn branch_from_head(text: &str) -> Option<String> {
    let target = text.trim().strip_prefix("ref:")?.trim();
    let branch = target.strip_prefix("refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_owned())
}

/// Finds the checkout containing `dir` (canonical): `None` outside git.
fn find_checkout(dir: &Path) -> Option<Checkout> {
    let root = projects::git_root(dir)?;
    let dot_git = root.join(".git");
    if fs::metadata(&dot_git).ok()?.is_dir() {
        return Some(Checkout {
            kind: PlacementKind::Main,
            head: dot_git.join("HEAD"),
            root,
        });
    }
    // A worktree or a submodule: `.git` is a file pointing at the private git directory.
    let git_dir = read_gitdir_file(&dot_git, &root)?;
    let kind = if git_dir.join("commondir").is_file() {
        PlacementKind::Worktree
    } else {
        PlacementKind::Main
    };
    Some(Checkout {
        kind,
        head: git_dir.join("HEAD"),
        root,
    })
}

/// The `gitdir: <path>` of a `.git` file, resolved against the checkout root.
fn read_gitdir_file(dot_git: &Path, root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(|| root.join(target))
}

/// The main checkout of the repository the checkout at `root` belongs to: `root` itself for a
/// main checkout, the repository's own top level for a linked worktree.
pub fn main_checkout_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if fs::metadata(&dot_git).ok()?.is_dir() {
        return fs::canonicalize(root).ok();
    }
    let git_dir = read_gitdir_file(&dot_git, root)?;
    let Ok(common) = fs::read_to_string(git_dir.join("commondir")) else {
        // A submodule: its own checkout is the repository.
        return fs::canonicalize(root).ok();
    };
    let common_dir = fs::canonicalize(git_dir.join(common.trim())).ok()?;
    // `<main>/.git`; a bare repository has no checkout to name.
    (common_dir.file_name()? == ".git")
        .then(|| common_dir.parent().map(Path::to_path_buf))
        .flatten()
}

/// Remembers the checkout of the directories sessions work in.
#[derive(Debug, Default)]
pub struct PlacementCache {
    dirs: HashMap<PathBuf, (Instant, Option<Checkout>)>,
}

impl PlacementCache {
    /// The checkout containing `dir`, from the cache when it knows the directory. Directories
    /// that cannot be resolved (gone) are not remembered.
    pub fn checkout(&mut self, dir: &Path) -> Option<Checkout> {
        let now = Instant::now();
        if let Some((at, found)) = self.dirs.get(dir)
            && now.duration_since(*at) < CACHE_TTL
        {
            return found.clone();
        }
        let canonical = fs::canonicalize(dir).ok()?;
        let found = find_checkout(&canonical);
        if self.dirs.len() >= CACHE_LIMIT {
            self.dirs.clear();
        }
        self.dirs.insert(dir.to_path_buf(), (now, found.clone()));
        found
    }

    /// Forgets everything (a worktree was created or removed).
    pub fn clear(&mut self) {
        self.dirs.clear();
    }

    /// The placement of a session working in `dir`, without sharing.
    pub fn placement(&mut self, dir: &Path) -> Option<Placement> {
        self.checkout(dir).map(|checkout| checkout.placement())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.dirs.len()
    }
}

/// What [`shared_with`] needs to know about one session.
#[derive(Debug, Clone, Copy)]
pub struct Occupant<'a> {
    pub id: SessionId,
    /// The session's process (or chat agent) is running.
    pub live: bool,
    /// An agent is reported in the session (`SessionInfo.agent`).
    pub agent: bool,
    /// The checkout root of the session's placement.
    pub root: Option<&'a Path>,
}

/// For every occupant the other live sessions with an agent in its checkout, while it has an
/// agent too: two agents writing to one checkout. Order follows the input. A shell next to an
/// agent shares nothing; only agents write on their own.
pub fn shared_with(occupants: &[Occupant<'_>]) -> Vec<Vec<SessionId>> {
    let active = |o: &Occupant<'_>| o.live && o.agent;
    occupants
        .iter()
        .map(|me| {
            let Some(root) = me.root.filter(|_| active(me)) else {
                return Vec::new();
            };
            occupants
                .iter()
                .filter(|other| other.id != me.id && active(other) && other.root == Some(root))
                .map(|other| other.id)
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

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

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
        (dir, root)
    }

    #[test]
    fn head_files_name_a_branch_or_none() {
        assert_eq!(
            branch_from_head("ref: refs/heads/feature/x\n"),
            Some("feature/x".to_owned())
        );
        assert_eq!(branch_from_head("ref: refs/remotes/o/x\n"), None);
        assert_eq!(
            branch_from_head("3f786850e387550fdab836ed7e6dc881de23001b\n"),
            None
        );
        assert_eq!(branch_from_head("ref: refs/heads/\n"), None);
        assert_eq!(branch_from_head(""), None);
    }

    #[test]
    fn a_directory_in_a_main_checkout_places_there_with_its_branch() {
        let (_dir, root) = repo();
        let deep = root.join("src/inner");
        fs::create_dir_all(&deep).unwrap();
        let placement = PlacementCache::default().placement(&deep).unwrap();
        assert_eq!(placement.kind, PlacementKind::Main);
        assert_eq!(placement.root, root);
        assert_eq!(placement.branch.as_deref(), Some("main"));
        assert!(placement.shared_with.is_empty());
        assert_eq!(main_checkout_of(&root), Some(root));
    }

    #[test]
    fn a_linked_worktree_places_as_worktree_and_knows_its_main_checkout() {
        let (dir, root) = repo();
        let wt = root.parent().unwrap().join("repo-wt");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature/x",
                wt.to_str().unwrap(),
            ],
        );
        let sub = wt.join("nested");
        fs::create_dir(&sub).unwrap();
        let placement = PlacementCache::default().placement(&sub).unwrap();
        assert_eq!(placement.kind, PlacementKind::Worktree);
        assert_eq!(placement.root, wt);
        assert_eq!(placement.branch.as_deref(), Some("feature/x"));
        assert_eq!(main_checkout_of(&wt), Some(root.clone()));

        // A detached HEAD has no branch.
        git(&wt, &["checkout", "-q", "--detach"]);
        assert_eq!(
            PlacementCache::default().placement(&wt).unwrap().branch,
            None
        );
        drop(dir);
    }

    #[test]
    fn a_symlinked_directory_resolves_and_plain_directories_have_no_placement() {
        let (dir, root) = repo();
        let link = root.parent().unwrap().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let mut cache = PlacementCache::default();
        assert_eq!(cache.placement(&link).unwrap().root, root);

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(cache.placement(plain.path()), None);
        assert_eq!(cache.placement(&dir.path().join("missing")), None);
    }

    #[test]
    fn the_cache_remembers_directories_until_cleared_and_never_the_missing() {
        let (_dir, root) = repo();
        let mut cache = PlacementCache::default();
        cache.checkout(&root).unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.checkout(&root.join("missing")), None);
        assert_eq!(cache.len(), 1, "a missing directory is not remembered");
        // A cached answer survives the directory's `.git` going away: no filesystem walk.
        fs::rename(root.join(".git"), root.join(".git-away")).unwrap();
        assert!(cache.checkout(&root).is_some());
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.checkout(&root).is_none());
    }

    #[test]
    fn the_branch_is_read_again_at_every_derivation() {
        let (_dir, root) = repo();
        let mut cache = PlacementCache::default();
        assert_eq!(
            cache.placement(&root).unwrap().branch.as_deref(),
            Some("main")
        );
        git(&root, &["checkout", "-q", "-b", "other"]);
        assert_eq!(
            cache.placement(&root).unwrap().branch.as_deref(),
            Some("other")
        );
    }

    fn ids() -> [SessionId; 3] {
        [SessionId::new(), SessionId::new(), SessionId::new()]
    }

    #[test]
    fn two_agents_in_one_checkout_share_it_and_a_stopped_one_clears_it() {
        let [a, b, c] = ids();
        let root = Path::new("/repo");
        let elsewhere = Path::new("/repo-wt/x");
        let occupant = |id, live, agent, root| Occupant {
            id,
            live,
            agent,
            root,
        };
        let both = [
            occupant(a, true, true, Some(root)),
            occupant(b, true, true, Some(root)),
            occupant(c, true, true, Some(elsewhere)),
        ];
        assert_eq!(shared_with(&both), [vec![b], vec![a], vec![]]);

        // One stops: the other is alone.
        let stopped = [
            occupant(a, true, true, Some(root)),
            occupant(b, false, false, Some(root)),
            occupant(c, true, true, Some(elsewhere)),
        ];
        assert_eq!(shared_with(&stopped), [vec![], vec![], vec![]]);

        // A shell without an agent neither shares nor is shared with; no placement shares none.
        let shell = [
            occupant(a, true, true, Some(root)),
            occupant(b, true, false, Some(root)),
            occupant(c, true, true, None),
        ];
        assert_eq!(shared_with(&shell), [vec![], vec![], vec![]]);

        // Three agents: each lists the other two, in input order.
        let three = [
            occupant(a, true, true, Some(root)),
            occupant(b, true, true, Some(root)),
            occupant(c, true, true, Some(root)),
        ];
        assert_eq!(shared_with(&three), [vec![b, c], vec![a, c], vec![a, b]]);
    }
}
