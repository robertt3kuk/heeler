//! Automatic shell integration: OSC 133 prompt marks and OSC 7 working directory for zsh, bash
//! and fish sessions, injected through environment variables and arguments only (the user's
//! own startup files are never edited; see `docs/shell-integration.md`).
//!
//! The scripts under `shell-integration/` are embedded at build time and written by [`install`]
//! to `<home>/shell-integration/<VERSION>/`. [`inject`] then points a freshly built command at
//! them:
//!
//! * **zsh** — `ZDOTDIR` names a shim directory whose `.zshenv` restores the user's `ZDOTDIR`,
//!   runs the user's `.zshenv` and loads the integration from the first `precmd`.
//! * **bash** — started as `bash --rcfile <shim>`; the shim replays the startup files bash
//!   would have read and then installs the hooks.
//! * **fish** — `XDG_DATA_DIRS` gets the integration directory prepended; fish loads its
//!   `fish/vendor_conf.d` snippet, which hands `XDG_DATA_DIRS` back to the user.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use crate::error::TerminalError;

/// Revision of the injected scripts, exported as `STAFFY_SHELL_INTEGRATION_VERSION` and part
/// of the install path. Bump it when a script changes in a way later slices must tell apart.
pub const VERSION: &str = "1";

/// Session environment variable that disables the integration when set to `0`.
pub const OPT_OUT_ENV: &str = "STAFFY_SHELL_INTEGRATION";

const ENV_DIR: &str = "STAFFY_SHELL_INTEGRATION_DIR";
const ENV_VERSION: &str = "STAFFY_SHELL_INTEGRATION_VERSION";
const ENV_ORIG_ZDOTDIR: &str = "STAFFY_ORIG_ZDOTDIR";
const ENV_ORIG_XDG: &str = "STAFFY_ORIG_XDG_DATA_DIRS";
const ENV_XDG_UNSET: &str = "STAFFY_XDG_DATA_DIRS_UNSET";
const ENV_BASH_INJECT: &str = "STAFFY_BASH_INJECT";
const ENV_BASH_RCFILE: &str = "STAFFY_BASH_RCFILE";

/// Variables the injection owns. They are cleared first so a daemon started from inside a
/// Staffy terminal cannot leak a stale injection into sessions that get none.
const OWNED_ENV: [&str; 7] = [
    ENV_DIR,
    ENV_VERSION,
    ENV_ORIG_ZDOTDIR,
    ENV_ORIG_XDG,
    ENV_XDG_UNSET,
    ENV_BASH_INJECT,
    ENV_BASH_RCFILE,
];

/// fish's default when `XDG_DATA_DIRS` is unset (XDG Base Directory specification).
const XDG_DEFAULT_DATA_DIRS: &str = "/usr/local/share:/usr/share";

const BASH_SHIM: &str = "bash/staffy.bash";

const FILES: [(&str, &str); 4] = [
    (
        "zsh/.zshenv",
        include_str!("../shell-integration/zsh/.zshenv"),
    ),
    (
        "zsh/staffy-integration.zsh",
        include_str!("../shell-integration/zsh/staffy-integration.zsh"),
    ),
    (
        BASH_SHIM,
        include_str!("../shell-integration/bash/staffy.bash"),
    ),
    (
        "fish/vendor_conf.d/staffy-shell-integration.fish",
        include_str!("../shell-integration/fish/vendor_conf.d/staffy-shell-integration.fish"),
    ),
];

/// Writes the embedded scripts to `<home>/shell-integration/<VERSION>/` and returns that
/// directory. Files whose content already matches are left alone; changed files are replaced
/// atomically, so shells that are starting right now never read a half-written script.
pub fn install(home: &Path) -> Result<PathBuf, TerminalError> {
    let root = home.join("shell-integration").join(VERSION);
    for (relative, content) in FILES {
        let path = root.join(relative);
        write_if_changed(&path, content).map_err(|source| TerminalError::ShellIntegration {
            path: path.clone(),
            source,
        })?;
    }
    Ok(root)
}

fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == content.as_bytes()) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Zsh,
    Bash,
    Fish,
}

fn shell_of(program: &OsStr) -> Option<Shell> {
    // A leading `-` is how login shells see themselves in `argv[0]`.
    let name = Path::new(program).file_name()?.to_str()?;
    match name.strip_prefix('-').unwrap_or(name) {
        "zsh" => Some(Shell::Zsh),
        "bash" => Some(Shell::Bash),
        "fish" => Some(Shell::Fish),
        _ => None,
    }
}

/// Points `cmd` at the integration under `root` when it runs zsh, bash or fish. Any other
/// program, sessions that opted out with `STAFFY_SHELL_INTEGRATION=0`, and `root == None` get
/// no injection; in every case stale injection variables inherited from the daemon's
/// environment are removed. `cwd` is the directory the process starts in: an injected shell
/// reports its working directory, so it must call it by the path the session asked for rather
/// than the canonical one (`/var/…` vs `/private/var/…` on macOS); shells take `$PWD` as
/// the logical path when it names the directory they start in.
pub(crate) fn inject(cmd: &mut CommandBuilder, root: Option<&Path>, cwd: Option<&Path>) {
    for key in OWNED_ENV {
        cmd.env_remove(key);
    }
    let Some(root) = root else {
        return;
    };
    if cmd.get_env(OPT_OUT_ENV) == Some(OsStr::new("0")) {
        return;
    }
    let Some(shell) = cmd.get_argv().first().and_then(|p| shell_of(p)) else {
        return;
    };
    match shell {
        Shell::Zsh => inject_zsh(cmd, root),
        Shell::Fish => inject_fish(cmd, root),
        Shell::Bash => {
            let Some(plan) = BashPlan::parse(&cmd.get_argv()[1..]) else {
                return;
            };
            inject_bash(cmd, root, plan);
        }
    }
    cmd.env(ENV_DIR, root);
    cmd.env(ENV_VERSION, VERSION);
    if let Some(cwd) = cwd.filter(|cwd| cwd.is_absolute()) {
        cmd.env("PWD", cwd);
    }
}

fn inject_zsh(cmd: &mut CommandBuilder, root: &Path) {
    // An empty ZDOTDIR makes zsh read nothing useful; treat it as unset like zsh's lookup of
    // the home directory does.
    if let Some(original) = cmd.get_env("ZDOTDIR").filter(|v| !v.is_empty()) {
        let original = original.to_owned();
        cmd.env(ENV_ORIG_ZDOTDIR, original);
    }
    cmd.env("ZDOTDIR", root.join("zsh"));
}

fn inject_fish(cmd: &mut CommandBuilder, root: &Path) {
    let mut dirs = root.as_os_str().to_owned();
    match cmd.get_env("XDG_DATA_DIRS").map(OsStr::to_owned) {
        Some(original) => {
            if !original.is_empty() {
                dirs.push(":");
                dirs.push(&original);
            } else {
                dirs.push(format!(":{XDG_DEFAULT_DATA_DIRS}"));
            }
            cmd.env(ENV_ORIG_XDG, original);
        }
        None => {
            // fish would fall back to the specification's default; keep that visible to it.
            dirs.push(format!(":{XDG_DEFAULT_DATA_DIRS}"));
            cmd.env(ENV_XDG_UNSET, "1");
        }
    }
    cmd.env("XDG_DATA_DIRS", dirs);
}

fn inject_bash(cmd: &mut CommandBuilder, root: &Path, plan: BashPlan) {
    let argv = cmd.get_argv_mut();
    let program = argv.remove(0);
    argv.clear();
    argv.push(program);
    argv.push("--rcfile".into());
    argv.push(root.join(BASH_SHIM).into_os_string());
    argv.extend(plan.kept_args.into_iter().map(Into::into));
    cmd.env(ENV_BASH_INJECT, plan.flags.join(" "));
    if let Some(rcfile) = plan.rcfile {
        cmd.env(ENV_BASH_RCFILE, rcfile);
    }
}

/// What the original bash command line asked for, reduced to what the shim must replay.
#[derive(Debug, PartialEq, Eq)]
struct BashPlan {
    /// `login`, `norc`, `noprofile`.
    flags: Vec<&'static str>,
    /// The user's own `--rcfile`/`--init-file`.
    rcfile: Option<String>,
    /// Arguments passed through to bash unchanged.
    kept_args: Vec<String>,
}

impl BashPlan {
    /// `None` when the command line is not a plain interactive start (a script, `-c`, posix
    /// mode, an option this code does not know): such invocations are left alone.
    fn parse(args: &[std::ffi::OsString]) -> Option<Self> {
        let mut plan = Self {
            flags: Vec::new(),
            rcfile: None,
            kept_args: Vec::new(),
        };
        let mut args = args.iter().map(|arg| arg.to_str());
        while let Some(arg) = args.next() {
            match arg? {
                "-l" | "--login" => plan.flags.push("login"),
                "--norc" => plan.flags.push("norc"),
                "--noprofile" => plan.flags.push("noprofile"),
                "--rcfile" | "--init-file" => plan.rcfile = Some(args.next()??.to_owned()),
                "-i" | "--noediting" | "--verbose" => plan.kept_args.push(arg?.to_owned()),
                // `-il` and friends.
                cluster
                    if cluster.len() > 1
                        && cluster.starts_with('-')
                        && cluster[1..].chars().all(|c| c == 'i' || c == 'l') =>
                {
                    if cluster.contains('l') {
                        plan.flags.push("login");
                    }
                    if cluster.contains('i') {
                        plan.kept_args.push("-i".to_owned());
                    }
                }
                _ => return None,
            }
        }
        plan.flags.dedup();
        Some(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env_of(cmd: &CommandBuilder, key: &str) -> Option<String> {
        cmd.get_env(key).map(|v| v.to_string_lossy().into_owned())
    }

    fn argv_of(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn command(argv: &[&str]) -> CommandBuilder {
        CommandBuilder::from_argv(argv.iter().map(OsString::from).collect())
    }

    #[test]
    fn other_programs_get_nothing() {
        for program in ["cat", "/usr/bin/env", "vim", "nu", "sh", "zsh-syntax"] {
            let mut cmd = command(&[program]);
            cmd.env(ENV_DIR, "stale");
            cmd.env(ENV_ORIG_ZDOTDIR, "stale");
            inject(&mut cmd, Some(Path::new("/si")), None);
            assert_eq!(argv_of(&cmd), [program]);
            for key in OWNED_ENV {
                assert_eq!(env_of(&cmd, key), None, "{program}: {key}");
            }
        }
    }

    #[test]
    fn injected_shells_start_with_the_requested_logical_cwd() {
        let mut cmd = command(&["/bin/zsh"]);
        inject(&mut cmd, Some(Path::new("/si")), Some(Path::new("/var/x")));
        assert_eq!(env_of(&cmd, "PWD").as_deref(), Some("/var/x"));

        let mut cmd = command(&["/bin/cat"]);
        cmd.env("PWD", "/daemon");
        inject(&mut cmd, Some(Path::new("/si")), Some(Path::new("/var/x")));
        assert_eq!(env_of(&cmd, "PWD").as_deref(), Some("/daemon"));

        let mut cmd = command(&["/bin/zsh"]);
        cmd.env("PWD", "/daemon");
        inject(
            &mut cmd,
            Some(Path::new("/si")),
            Some(Path::new("relative")),
        );
        assert_eq!(env_of(&cmd, "PWD").as_deref(), Some("/daemon"));
    }

    #[test]
    fn opt_out_is_honoured_for_shells() {
        let mut cmd = command(&["/bin/zsh", "-l"]);
        cmd.env(OPT_OUT_ENV, "0");
        cmd.env("ZDOTDIR", "/user");
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(env_of(&cmd, "ZDOTDIR").as_deref(), Some("/user"));
        assert_eq!(env_of(&cmd, ENV_DIR), None);
    }

    #[test]
    fn zsh_keeps_the_original_zdotdir() {
        let mut cmd = command(&["/bin/zsh", "-l"]);
        cmd.env("ZDOTDIR", "/user/zd");
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(env_of(&cmd, "ZDOTDIR").as_deref(), Some("/si/zsh"));
        assert_eq!(env_of(&cmd, ENV_ORIG_ZDOTDIR).as_deref(), Some("/user/zd"));
        assert_eq!(env_of(&cmd, ENV_DIR).as_deref(), Some("/si"));
        assert_eq!(env_of(&cmd, ENV_VERSION).as_deref(), Some(VERSION));
        assert_eq!(argv_of(&cmd), ["/bin/zsh", "-l"]);
    }

    #[test]
    fn fish_prepends_and_remembers_xdg_data_dirs() {
        let mut cmd = command(&["fish"]);
        cmd.env("XDG_DATA_DIRS", "/a:/b");
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(env_of(&cmd, "XDG_DATA_DIRS").as_deref(), Some("/si:/a:/b"));
        assert_eq!(env_of(&cmd, ENV_ORIG_XDG).as_deref(), Some("/a:/b"));
        assert_eq!(env_of(&cmd, ENV_XDG_UNSET), None);

        let mut cmd = command(&["fish"]);
        cmd.env_remove("XDG_DATA_DIRS");
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(
            env_of(&cmd, "XDG_DATA_DIRS").as_deref(),
            Some("/si:/usr/local/share:/usr/share")
        );
        assert_eq!(env_of(&cmd, ENV_ORIG_XDG), None);
        assert_eq!(env_of(&cmd, ENV_XDG_UNSET).as_deref(), Some("1"));
    }

    #[test]
    fn bash_login_becomes_an_rcfile_start() {
        let mut cmd = command(&["/bin/bash", "-l"]);
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(
            argv_of(&cmd),
            ["/bin/bash", "--rcfile", "/si/bash/staffy.bash"]
        );
        assert_eq!(env_of(&cmd, ENV_BASH_INJECT).as_deref(), Some("login"));
        assert_eq!(env_of(&cmd, ENV_BASH_RCFILE), None);
    }

    #[test]
    fn bash_user_flags_move_into_the_environment() {
        let mut cmd = command(&["bash", "--norc", "--rcfile", "/u/rc", "-i", "--noediting"]);
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(
            argv_of(&cmd),
            [
                "bash",
                "--rcfile",
                "/si/bash/staffy.bash",
                "-i",
                "--noediting"
            ]
        );
        assert_eq!(env_of(&cmd, ENV_BASH_INJECT).as_deref(), Some("norc"));
        assert_eq!(env_of(&cmd, ENV_BASH_RCFILE).as_deref(), Some("/u/rc"));

        let mut cmd = command(&["bash", "-il"]);
        inject(&mut cmd, Some(Path::new("/si")), None);
        assert_eq!(env_of(&cmd, ENV_BASH_INJECT).as_deref(), Some("login"));
        assert_eq!(argv_of(&cmd)[3..], ["-i"]);
    }

    #[test]
    fn bash_scripts_commands_and_posix_mode_are_left_alone() {
        for args in [
            &["bash", "-c", "true"][..],
            &["bash", "script.sh"],
            &["bash", "--posix"],
            &["bash", "-x"],
            &["bash", "--rcfile"],
        ] {
            let mut cmd = command(args);
            inject(&mut cmd, Some(Path::new("/si")), None);
            assert_eq!(argv_of(&cmd), args, "{args:?}");
            assert_eq!(env_of(&cmd, ENV_DIR), None, "{args:?}");
        }
    }

    #[test]
    fn login_shell_names_are_recognised() {
        assert_eq!(shell_of(OsStr::new("-zsh")), Some(Shell::Zsh));
        assert_eq!(
            shell_of(OsStr::new("/opt/homebrew/bin/fish")),
            Some(Shell::Fish)
        );
        assert_eq!(shell_of(OsStr::new("/usr/bin/bash5")), None);
    }

    #[test]
    fn install_writes_versioned_scripts_and_is_idempotent() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = install(home.path()).expect("install");
        assert_eq!(root, home.path().join("shell-integration").join(VERSION));
        for (relative, content) in FILES {
            assert_eq!(
                fs::read_to_string(root.join(relative)).expect("read"),
                content
            );
        }
        // A stale script is replaced, an unchanged one is not rewritten.
        let shim = root.join(BASH_SHIM);
        fs::write(&shim, "stale").expect("write");
        let again = install(home.path()).expect("install again");
        assert_eq!(again, root);
        assert_eq!(fs::read_to_string(&shim).expect("read"), FILES[2].1);
    }
}
