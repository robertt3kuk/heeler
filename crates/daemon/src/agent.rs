//! Rules for the agent reporting channel (`agent.report` / `agent.release`, protocol 0.4):
//! what a reported resume command may look like, what is stored, and where `HUTAN_BIN` points.

use std::path::{Path, PathBuf};

use crate::restore::ResumeRef;

/// Most words (command included) of a reported resume command.
pub const MAX_RESUME_WORDS: usize = 64;
/// Most bytes of a reported resume command, summed over its words.
pub const MAX_RESUME_BYTES: usize = 8 * 1024;

/// Checks a reported resume command. The daemon runs it unattended after a restart, so it is
/// held to a conservative shape: a plain command name (looked up on `PATH`, never a path) and
/// arguments without apostrophes or control characters.
pub fn validate_resume_argv(argv: &[String]) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err("resume_argv must not be empty".to_owned());
    };
    if argv.len() > MAX_RESUME_WORDS {
        return Err(format!(
            "resume_argv has {} words; at most {MAX_RESUME_WORDS} are allowed",
            argv.len()
        ));
    }
    let bytes: usize = argv.iter().map(String::len).sum();
    if bytes > MAX_RESUME_BYTES {
        return Err(format!(
            "resume_argv is {bytes} bytes; at most {MAX_RESUME_BYTES} are allowed"
        ));
    }
    if !is_plain_command(program) {
        return Err(format!(
            "resume_argv[0] `{program}` must be a plain command name on PATH"
        ));
    }
    if argv
        .iter()
        .any(|word| word.chars().any(|c| c == '\'' || c.is_control()))
    {
        return Err("resume_argv must not contain apostrophes or control characters".to_owned());
    }
    Ok(())
}

/// Letters, digits, and `._+@-`, not starting with `-`: no path separators, no spaces.
fn is_plain_command(program: &str) -> bool {
    !program.is_empty()
        && !program.starts_with('-')
        && program
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '@' | '-'))
}

/// The `resume_ref_json` stored for a validated report.
pub fn resume_ref_json(
    source: &str,
    agent: &str,
    agent_session_id: Option<&str>,
    argv: Vec<String>,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&ResumeRef {
        argv,
        source: Some(source.to_owned()),
        agent: Some(agent.to_owned()),
        agent_session_id: agent_session_id.map(str::to_owned),
    })
}

/// The `hutan` CLI sessions get as `HUTAN_BIN`: the one next to this executable, else the
/// first on `PATH`, else the bare name for the shell to resolve.
pub fn resolve_hutan_bin() -> PathBuf {
    locate_hutan(
        std::env::current_exe().ok().as_deref(),
        std::env::var_os("PATH"),
    )
    .unwrap_or_else(|| PathBuf::from("hutan"))
}

/// The executable `hutan` beside `exe` (as invoked and with symlinks resolved), else the first
/// one on `path`. Inside an app bundle that is the bundled CLI next to `hutand`.
fn locate_hutan(exe: Option<&Path>, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let beside = exe
        .into_iter()
        .flat_map(|exe| [Some(exe.to_owned()), exe.canonicalize().ok()])
        .flatten()
        .filter_map(|exe| exe.parent().map(|dir| dir.join("hutan")));
    let on_path = path
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("hutan"));
    beside
        .chain(on_path)
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn the_daemon_of_an_app_bundle_gives_sessions_the_bundled_cli() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("Hutan.app/Contents/MacOS");
        for name in ["hutan-desktop", "hutand", "hutan"] {
            executable(&macos.join(name));
        }
        assert_eq!(
            locate_hutan(Some(&macos.join("hutand")), None),
            Some(macos.join("hutan"))
        );
        // Not found beside it: the PATH is next, then nothing.
        let lonely = dir.path().join("lonely/hutand");
        executable(&lonely);
        assert_eq!(locate_hutan(Some(&lonely), None), None);
        let installed = dir.path().join("installed");
        executable(&installed.join("hutan"));
        let path = std::env::join_paths([installed.clone()]).unwrap();
        assert_eq!(
            locate_hutan(Some(&lonely), Some(path)),
            Some(installed.join("hutan"))
        );
    }

    #[test]
    fn accepts_the_resume_commands_of_the_known_agents() {
        for words in [
            &["codex", "resume", "0198-abc"][..],
            &["claude", "--resume", "5f3e1c1a-8d51"],
            &["omp", "--resume=0f4c7d52"],
            &["opencode", "--session", "ses_1"],
            &["my-agent_2.0", "say \"hi\" there"],
        ] {
            assert_eq!(validate_resume_argv(&argv(words)), Ok(()), "{words:?}");
        }
    }

    #[test]
    fn rejects_unsafe_or_oversized_commands() {
        let long = vec!["x".to_owned(); MAX_RESUME_WORDS + 1];
        let mut at_limit = vec!["x".to_owned(); MAX_RESUME_WORDS];
        assert_eq!(validate_resume_argv(&at_limit), Ok(()));
        at_limit[1] = "y".repeat(MAX_RESUME_BYTES);
        let cases = [
            Vec::new(),
            argv(&[""]),
            argv(&["/usr/bin/codex", "resume"]),
            argv(&["./codex"]),
            argv(&["-codex"]),
            argv(&["co dex"]),
            argv(&["codex", "it's"]),
            argv(&["codex", "bell\u{7}"]),
            argv(&["codex", "line\nbreak"]),
            long,
            at_limit,
        ];
        for case in cases {
            assert!(validate_resume_argv(&case).is_err(), "{case:?}");
        }
    }

    #[test]
    fn stored_reference_round_trips_through_the_restore_reader() {
        let json = resume_ref_json(
            "claude-hooks",
            "claude",
            Some("abc"),
            argv(&["claude", "-r"]),
        )
        .unwrap();
        assert_eq!(
            crate::restore::resume_argv(&json),
            Some(argv(&["claude", "-r"]))
        );
        assert_eq!(
            crate::restore::resume_source(&json).as_deref(),
            Some("claude-hooks")
        );
    }
}
