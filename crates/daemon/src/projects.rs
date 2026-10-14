//! Filesystem rules of projects and workspaces: the Home folder, resolving a folder to a project
//! root, and which workspace root contains a path. Blocking functions; callers run them off the
//! async threads (or, for [`root_contains`], accept a few metadata calls).

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use heeler_protocol::control::{ErrorBody, ErrorCode};

/// Creates the Home folder (mode `0700`, with missing parents) unless it exists, and returns
/// its canonical path. An existing folder is never touched and nothing is created inside it.
pub fn ensure_home_folder(folder: &Path) -> io::Result<PathBuf> {
    match fs::metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a directory", folder.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)?;
        }
        Err(e) => return Err(e),
    }
    fs::canonicalize(folder)
}

/// A folder as `workspace.open` and `workspace.add_root` see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// The requested folder, canonical (symlinks resolved).
    pub path: PathBuf,
    /// The git repository root containing `path`, else `path` itself.
    pub root: PathBuf,
}

impl Folder {
    /// Display name of the project: the root's folder name.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.root.display().to_string())
    }
}

/// Canonicalises `path` (absolute, existing directory) and finds its repository root.
pub fn resolve_folder(path: &Path) -> Result<Folder, ErrorBody> {
    if !path.is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{}` is not an absolute path", path.display()),
        ));
    }
    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("{} does not exist", path.display()),
            ));
        }
        Err(e) => {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("cannot use {}: {e}", path.display()),
            ));
        }
    };
    if !canonical.is_dir() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("{} is not a directory", path.display()),
        ));
    }
    let root = git_root(&canonical).unwrap_or_else(|| canonical.clone());
    Ok(Folder {
        path: canonical,
        root,
    })
}

/// The nearest ancestor of `start` (or `start` itself) holding a `.git` entry: a directory for a
/// repository, a file for a worktree or submodule.
pub fn git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .map(Path::to_path_buf)
}

/// `path` (canonical) is `root` or lies below it. A stored root may reach the same place
/// through a symlink, so its canonical form counts too.
pub fn root_contains(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        || fs::canonicalize(root).is_ok_and(|canonical| path.starts_with(canonical))
}

/// Both paths name the same place, given as written or after resolving symlinks.
pub fn same_place(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn canonical(dir: &tempfile::TempDir) -> PathBuf {
        fs::canonicalize(dir.path()).unwrap()
    }

    #[test]
    fn the_home_folder_is_created_private_once_and_left_alone_after() {
        let dir = tempfile::tempdir().unwrap();
        let home = canonical(&dir).join("deep/Heeler");
        let made = ensure_home_folder(&home).unwrap();
        assert_eq!(made, home);
        let mode = fs::metadata(&home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        // An existing folder keeps its mode and gets no new files.
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_home_folder(&home).unwrap();
        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);

        let file = canonical(&dir).join("file");
        fs::write(&file, b"x").unwrap();
        assert!(ensure_home_folder(&file).is_err());
    }

    #[test]
    fn a_folder_inside_a_repository_resolves_to_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let repo = base.join("repo");
        let deep = repo.join("src/inner");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();

        let folder = resolve_folder(&deep).unwrap();
        assert_eq!(folder.path, deep);
        assert_eq!(folder.root, repo);
        assert_eq!(folder.name(), "repo");

        // A `.git` file (worktree, submodule) marks a root as well.
        let worktree = base.join("wt");
        fs::create_dir(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: /elsewhere").unwrap();
        assert_eq!(resolve_folder(&worktree).unwrap().root, worktree);

        // Outside any repository the folder is its own root.
        let plain = base.join("plain");
        fs::create_dir(&plain).unwrap();
        let folder = resolve_folder(&plain).unwrap();
        assert_eq!(folder.name(), "plain");
        assert_eq!(folder.root, plain);
    }

    #[test]
    fn symlinks_resolve_and_bad_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        assert_eq!(resolve_folder(&base.join("link")).unwrap().path, real);

        assert_eq!(
            resolve_folder(Path::new("relative")).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            resolve_folder(&base.join("missing")).unwrap_err().code,
            ErrorCode::NotFound
        );
        let file = base.join("file");
        fs::write(&file, b"x").unwrap();
        assert_eq!(
            resolve_folder(&file).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn containment_is_by_path_component_and_follows_symlinked_roots() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let real = base.join("real");
        fs::create_dir_all(real.join("sub")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(root_contains(&real, &real));
        assert!(root_contains(&real, &real.join("sub")));
        assert!(
            root_contains(&link, &real.join("sub")),
            "root given by symlink"
        );
        assert!(
            !root_contains(&base.join("rea"), &real),
            "not a string prefix"
        );
        assert!(!root_contains(&real.join("sub"), &real));
        assert!(same_place(&link, &real));
        assert!(!same_place(&link, &real.join("sub")));
    }
}
