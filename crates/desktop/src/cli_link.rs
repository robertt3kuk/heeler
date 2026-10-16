//! "Add `hutan` to your PATH": the symlink `~/.local/bin/hutan` → `Contents/MacOS/hutan` of the
//! running `Hutan.app`, offered by the first-run setup's Command line step (design.md § First-run
//! setup).
//!
//! The decision is separate from the effect: [`CliLink::plan`] looks at the folder and says what
//! making the link would do; [`CliLink::apply`] only ever does what the plan allowed. Hutan
//! creates the link when the folder exists and the name is free, or already is this link. It
//! never replaces a file or another link: then the user gets the command to run themselves.
//! Everything takes its paths from the [`CliLink`], so tests use temporary folders and never
//! touch `$HOME`.

use std::io;
use std::path::{Path, PathBuf};

use crate::macos::login_item::inside_app_bundle;

/// The name of the link, and of the executable it points to.
pub const NAME: &str = "hutan";

/// Where the link goes and what it points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliLink {
    /// The folder the link is made in (`~/.local/bin`).
    pub bin_dir: PathBuf,
    /// The executable it points to (`Hutan.app/Contents/MacOS/hutan`).
    pub target: PathBuf,
}

/// Why Hutan leaves the link to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manual {
    /// The folder does not exist (making a folder on someone's PATH is their call).
    NoFolder,
    /// The name is taken: by a file, or by a link that points somewhere else.
    Taken,
    /// There is no `hutan` where the link would point.
    NoTarget,
}

/// What making the link would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The link does not exist and will be created.
    Create,
    /// The link exists and points to this executable already.
    AlreadyLinked,
    /// Hutan does not touch it; `command` makes the link when run in a terminal.
    Manual { why: Manual, command: String },
}

impl CliLink {
    /// The link for the running desktop: `~/.local/bin/hutan` → the `hutan` beside this
    /// executable, when the desktop runs from `Hutan.app` and that executable exists.
    /// `None` otherwise (a `cargo run` desktop has no bundle to point into) and when the home
    /// folder is unknown.
    pub fn system() -> Option<Self> {
        let home = std::env::home_dir()?;
        Self::for_executable(&std::env::current_exe().ok()?, &home)
    }

    /// [`system`](Self::system) for the desktop executable `exe` and the home folder `home`.
    pub fn for_executable(exe: &Path, home: &Path) -> Option<Self> {
        if !inside_app_bundle(exe) {
            return None;
        }
        let target = exe.parent()?.join(NAME);
        target.is_file().then(|| Self {
            bin_dir: home.join(".local").join("bin"),
            target,
        })
    }

    /// Where the link is.
    pub fn link(&self) -> PathBuf {
        self.bin_dir.join(NAME)
    }

    /// What making the link would do now.
    pub fn plan(&self) -> Plan {
        let manual = |why| Plan::Manual {
            why,
            command: self.command(),
        };
        if !self.target.is_file() {
            return manual(Manual::NoTarget);
        }
        if !self.bin_dir.is_dir() {
            return manual(Manual::NoFolder);
        }
        match std::fs::symlink_metadata(self.link()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Plan::Create,
            Err(_) => manual(Manual::Taken),
            Ok(meta) if meta.is_symlink() => match std::fs::read_link(self.link()) {
                Ok(destination) if destination == self.target => Plan::AlreadyLinked,
                _ => manual(Manual::Taken),
            },
            Ok(_) => manual(Manual::Taken),
        }
    }

    /// The command that makes the link by hand: it makes the folder when it is missing and
    /// replaces what is there (`-f`) when the name is taken, which is the user's decision.
    pub fn command(&self) -> String {
        let link = self.link();
        let make = format!("ln -sf {} {}", quote(&self.target), quote(&link));
        if self.bin_dir.is_dir() {
            make
        } else {
            format!("mkdir -p {} && {make}", quote(&self.bin_dir))
        }
    }

    /// Makes the link if [`plan`](Self::plan) says so. Returns the plan that was followed: `Create`
    /// (now done), `AlreadyLinked` or `Manual` (nothing was touched).
    pub fn apply(&self) -> io::Result<Plan> {
        let plan = self.plan();
        if plan == Plan::Create {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&self.target, self.link())?;
            #[cfg(not(unix))]
            return Err(io::Error::other("links need a Unix system"));
        }
        Ok(plan)
    }
}

/// `path` as one shell word.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder with a `bin` folder and a fake `Hutan.app` beside it.
    struct Sandbox {
        dir: tempfile::TempDir,
        link: CliLink,
    }

    fn sandbox() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("Hutan.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("hutan"), "").unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let link = CliLink {
            bin_dir,
            target: macos.join("hutan"),
        };
        Sandbox { dir, link }
    }

    fn why(plan: Plan) -> Manual {
        match plan {
            Plan::Manual { why, .. } => why,
            other => panic!("not manual: {other:?}"),
        }
    }

    #[test]
    fn a_free_name_in_an_existing_folder_is_made() {
        let s = sandbox();
        assert_eq!(s.link.plan(), Plan::Create);
        assert_eq!(s.link.apply().unwrap(), Plan::Create);
        assert_eq!(std::fs::read_link(s.link.link()).unwrap(), s.link.target);
        // Again: nothing to do, nothing changes.
        assert_eq!(s.link.plan(), Plan::AlreadyLinked);
        assert_eq!(s.link.apply().unwrap(), Plan::AlreadyLinked);
        assert_eq!(std::fs::read_link(s.link.link()).unwrap(), s.link.target);
    }

    #[test]
    fn a_foreign_file_or_link_is_left_alone() {
        let s = sandbox();
        std::fs::write(s.link.link(), "#!/bin/sh\n").unwrap();
        assert_eq!(why(s.link.plan()), Manual::Taken);
        assert!(matches!(s.link.apply().unwrap(), Plan::Manual { .. }));
        assert_eq!(
            std::fs::read_to_string(s.link.link()).unwrap(),
            "#!/bin/sh\n",
            "the file is untouched"
        );

        // A link to somewhere else, even a dangling one, is not ours either.
        std::fs::remove_file(s.link.link()).unwrap();
        std::os::unix::fs::symlink(s.dir.path().join("elsewhere"), s.link.link()).unwrap();
        assert_eq!(why(s.link.plan()), Manual::Taken);
        assert_eq!(s.link.apply().unwrap().clone(), s.link.plan());
        assert_eq!(
            std::fs::read_link(s.link.link()).unwrap(),
            s.dir.path().join("elsewhere")
        );
    }

    #[test]
    fn a_missing_folder_is_not_made_and_the_command_makes_it() {
        let mut s = sandbox();
        s.link.bin_dir = s.dir.path().join("no/such/bin");
        let plan = s.link.plan();
        let Plan::Manual { why, command } = plan else {
            panic!("expected manual");
        };
        assert_eq!(why, Manual::NoFolder);
        assert!(command.starts_with("mkdir -p '"), "{command}");
        assert!(command.contains("&& ln -sf '"), "{command}");
        assert!(s.link.apply().is_ok());
        assert!(!s.link.bin_dir.exists(), "nothing was created");
    }

    #[test]
    fn the_command_quotes_odd_paths() {
        let mut s = sandbox();
        s.link.target = s.dir.path().join("it's here/hutan");
        let command = s.link.command();
        assert!(command.starts_with("ln -sf '"), "{command}");
        assert!(command.contains(r"it'\''s here"), "{command}");
    }

    #[test]
    fn a_target_that_is_missing_is_reported() {
        let mut s = sandbox();
        s.link.target = s.dir.path().join("gone/hutan");
        assert_eq!(why(s.link.plan()), Manual::NoTarget);
    }

    #[test]
    fn only_a_desktop_in_an_app_bundle_has_a_link() {
        let s = sandbox();
        let home = s.dir.path().join("home");
        let exe = s.dir.path().join("Hutan.app/Contents/MacOS/hutan-desktop");
        let link = CliLink::for_executable(&exe, &home).unwrap();
        assert_eq!(link.bin_dir, home.join(".local/bin"));
        assert_eq!(link.target, s.link.target);
        // Outside a bundle, or with no `hutan` beside it, there is nothing to link to.
        assert_eq!(
            CliLink::for_executable(&s.dir.path().join("target/debug/hutan-desktop"), &home),
            None
        );
        std::fs::remove_file(&s.link.target).unwrap();
        assert_eq!(CliLink::for_executable(&exe, &home), None);
    }
}
