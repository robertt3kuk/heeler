//! The user's login environment, as their login shell builds it (the "resolve login shell
//! environment" pattern of Zed and VS Code).
//!
//! A session that runs a login shell lets the shell build its own environment. A session with an
//! explicit command (`hutan new -- omp`, a resumed agent) has no shell to do that, and the agents
//! it runs usually live where only the user's shell configuration puts them (`~/.local/bin`,
//! Homebrew, mise/nvm shims). So the daemon runs the login shell once, `<shell> -l -c
//! '/usr/bin/env -0'`, from the base environment and keeps what it prints: [`capture`] runs it,
//! [`parse_env0`] and [`keep_captured`] turn the output into variables, [`LoginEnv`] caches the
//! result per shell and refreshes it in the background.
//!
//! The shell's output is parsed, never evaluated, and never logged: failures say which shell and
//! why, not what was printed (values may be secrets).

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::sys::{self, Signal};

/// How long the login shell may take to print its environment before it is killed.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
/// A capture older than this is redone in the background when a session starts.
pub const REFRESH_AFTER: Duration = Duration::from_secs(10 * 60);
/// A shell that has never been captured successfully is tried again after this long, when a
/// session starts (a slow or briefly stuck start must not disable the login environment for
/// the refresh interval).
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(10);
/// The longest a spawn waits for the very first capture of a shell before it starts from the
/// base environment alone.
pub const FIRST_WAIT: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(10);

/// The variables of NUL-separated `KEY=VALUE` output (`env -0`). Values may contain newlines and
/// `=`; entries without `=`, with an empty or non-identifier name, or that are not UTF-8 are
/// skipped (that drops exported bash functions, `BASH_FUNC_x%%`).
pub fn parse_env0(output: &[u8]) -> BTreeMap<String, String> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (name, value) = entry.split_once('=')?;
            is_identifier(name).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a captured variable is part of the user's environment. Dropped: what the shell
/// reports about itself or the run (`PWD`, `OLDPWD`, `SHLVL`, `_`), the terminal's own variables
/// (`TERM`, `TERM_PROGRAM*`, `COLORTERM`; the session sets them), and everything Hutan owns
/// (`HUTAN_*`, which includes every shell-integration variable).
pub fn keep_captured(name: &str) -> bool {
    !matches!(
        name,
        "PWD" | "OLDPWD" | "SHLVL" | "_" | "TERM" | "COLORTERM"
    ) && !name.starts_with("TERM_PROGRAM")
        && !name.starts_with("HUTAN_")
}

/// Creates an anonymous scratch file (mode `0600`, unlinked at once) for the shell's stdout. A
/// file rather than a pipe: a daemon the user's startup files leave behind inherits stdout and
/// would keep a pipe open forever.
fn scratch_file() -> std::io::Result<std::fs::File> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path: PathBuf = std::env::temp_dir().join(format!(
        ".hutan-login-env-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

/// Runs `shell -l -c '/usr/bin/env -0'` from `base` (an otherwise empty environment, in the
/// account's home directory, stdin `/dev/null`, no shell integration) and returns the variables
/// worth keeping ([`keep_captured`]). The shell and everything it started is killed after
/// `timeout`. The error says why it failed and never contains output.
pub fn capture(
    shell: &str,
    base: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<BTreeMap<String, String>, String> {
    let mut output = scratch_file().map_err(|e| format!("cannot create a scratch file: {e}"))?;
    let mut command = Command::new(shell);
    command
        .args(["-l", "-c", "/usr/bin/env -0"])
        .env_clear()
        .envs(base)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            output
                .try_clone()
                .map_err(|e| format!("cannot share the scratch file: {e}"))?,
        ))
        .stderr(Stdio::null())
        // Its own process group, so a timeout takes the whole tree down.
        .process_group(0);
    if let Some(home) = base.get("HOME").filter(|home| Path::new(home).is_dir()) {
        command.current_dir(home);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start the shell: {e}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = sys::signal_group(child.id(), Signal::Kill);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", timeout.as_secs_f32()));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(format!("cannot wait for the shell: {e}")),
        }
    };
    if !status.success() {
        return Err(format!("the shell exited with {status}"));
    }
    let mut bytes = Vec::new();
    output
        .seek(SeekFrom::Start(0))
        .and_then(|_| output.read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read the shell's output: {e}"))?;
    let env: BTreeMap<String, String> = parse_env0(&bytes)
        .into_iter()
        .filter(|(name, _)| keep_captured(name))
        .collect();
    if env.is_empty() {
        return Err("the shell printed no environment".to_owned());
    }
    Ok(env)
}

#[derive(Debug, Default)]
struct Slot {
    /// The last good capture.
    env: Option<BTreeMap<String, String>>,
    /// When the last capture attempt (good or not) finished; `None` before the first.
    at: Option<Instant>,
    in_flight: bool,
}

#[derive(Debug)]
struct Inner {
    slots: Mutex<BTreeMap<String, Slot>>,
    finished: Condvar,
    capture_timeout: Duration,
    refresh_after: Duration,
    retry_after_failure: Duration,
    first_wait: Duration,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Captured login environments, per shell path. A spawn never waits for a refresh; only the
/// first capture of a shell is waited for, for at most [`FIRST_WAIT`].
#[derive(Debug, Clone)]
pub struct LoginEnv {
    inner: Arc<Inner>,
}

impl Default for LoginEnv {
    fn default() -> Self {
        Self::with_limits(
            CAPTURE_TIMEOUT,
            REFRESH_AFTER,
            RETRY_AFTER_FAILURE,
            FIRST_WAIT,
        )
    }
}

impl LoginEnv {
    pub fn with_limits(
        capture_timeout: Duration,
        refresh_after: Duration,
        retry_after_failure: Duration,
        first_wait: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                slots: Mutex::new(BTreeMap::new()),
                finished: Condvar::new(),
                capture_timeout,
                refresh_after,
                retry_after_failure,
                first_wait,
            }),
        }
    }

    /// Starts a background capture for `shell` when there is none yet or it is stale.
    fn refresh_if_stale(&self, shell: &str, base: &BTreeMap<String, String>) {
        let mut slots = self.inner.lock();
        let slot = slots.entry(shell.to_owned()).or_default();
        let interval = if slot.env.is_some() {
            self.inner.refresh_after
        } else {
            self.inner.retry_after_failure
        };
        let stale = slot.at.is_none_or(|at| at.elapsed() >= interval);
        if !stale || slot.in_flight {
            return;
        }
        slot.in_flight = true;
        let inner = Arc::clone(&self.inner);
        let (name, base) = (shell.to_owned(), base.clone());
        let spawned = std::thread::Builder::new()
            .name("hutan-login-env".into())
            .spawn(move || {
                let result = capture(&name, &base, inner.capture_timeout);
                if let Err(reason) = &result {
                    tracing::warn!(shell = %name, %reason, "login environment capture failed; sessions start from the base environment");
                }
                let mut slots = inner.lock();
                let slot = slots.entry(name).or_default();
                if let Ok(env) = result {
                    slot.env = Some(env);
                }
                slot.at = Some(Instant::now());
                slot.in_flight = false;
                inner.finished.notify_all();
            });
        if let Err(error) = spawned {
            tracing::warn!(shell, %error, "cannot start the login environment capture");
            let slot = slots.entry(shell.to_owned()).or_default();
            slot.in_flight = false;
            slot.at = Some(Instant::now());
        }
    }

    /// Starts capturing `shell`'s login environment now, without waiting for it.
    pub fn request(&self, shell: &str, base: &BTreeMap<String, String>) {
        self.refresh_if_stale(shell, base);
    }

    /// The last good capture for `shell`. A stale capture is returned at once and refreshed in
    /// the background. With no good capture, this waits at most the first-wait limit for one
    /// that is running (the first, or a retry after a failure). `None` (start from the base
    /// environment) when there is none: still running after the wait, or not running and not
    /// yet due for a retry.
    pub fn get(
        &self,
        shell: &str,
        base: &BTreeMap<String, String>,
    ) -> Option<BTreeMap<String, String>> {
        self.refresh_if_stale(shell, base);
        let deadline = Instant::now() + self.inner.first_wait;
        let mut slots = self.inner.lock();
        loop {
            let slot = slots.get(shell)?;
            if slot.env.is_some() {
                return slot.env.clone();
            }
            let now = Instant::now();
            if !slot.in_flight || now >= deadline {
                return None;
            }
            slots = self
                .inner
                .finished
                .wait_timeout(slots, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A script that stands in for the login shell; it is run as `script -l -c '…'`.
    fn fake_shell(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // macOS vets the first execution of a new executable, which can take tens of seconds on
        // a loaded machine; do that here, outside the timeouts the tests are about.
        let _ = Command::new(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        path.to_string_lossy().into_owned()
    }

    fn base() -> BTreeMap<String, String> {
        map(&[("HOME", "/"), ("PATH", "/usr/bin:/bin"), ("USER", "ada")])
    }

    #[test]
    fn env0_output_keeps_multiline_values_and_equals_signs() {
        let output = b"A=1\0MULTI=line one\nline two\n\nlast\0EQ=a=b==c\0EMPTY=\0TRAILING=x\0";
        assert_eq!(
            parse_env0(output),
            map(&[
                ("A", "1"),
                ("MULTI", "line one\nline two\n\nlast"),
                ("EQ", "a=b==c"),
                ("EMPTY", ""),
                ("TRAILING", "x"),
            ])
        );
    }

    #[test]
    fn env0_garbage_is_skipped_not_fatal() {
        let mut output = b"GOOD=1\0no equals sign\0=nameless\0BASH_FUNC_f%%=() { :; }\0".to_vec();
        output.extend_from_slice(b"BAD=\xff\xfe\0");
        output.extend_from_slice(b"1STARTS_WITH_DIGIT=x\0LATER=2");
        assert_eq!(parse_env0(&output), map(&[("GOOD", "1"), ("LATER", "2")]));
        assert!(parse_env0(b"").is_empty());
        assert!(parse_env0(b"\0\0").is_empty());
    }

    #[test]
    fn the_drop_list_removes_shell_run_terminal_and_hutan_variables() {
        for dropped in [
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "COLORTERM",
            "HUTAN_SESSION_ID",
            "HUTAN_SHELL_INTEGRATION_DIR",
            "HUTAN_ORIG_ZDOTDIR",
            "HUTAN_BASH_INJECT",
        ] {
            assert!(!keep_captured(dropped), "{dropped} must be dropped");
        }
        for kept in [
            "PATH",
            "HOME",
            "HOMEBREW_PREFIX",
            "NVM_DIR",
            "JAVA_HOME",
            "SSH_AUTH_SOCK",
            "TERMINFO",
            "MYHUTAN_X",
            "ZDOTDIR",
        ] {
            assert!(keep_captured(kept), "{kept} must be kept");
        }
    }

    #[test]
    fn a_shell_is_run_as_a_login_shell_from_the_base_and_its_variables_are_filtered() {
        let dir = tempfile::tempdir().unwrap();
        // Prints its arguments and a few things about its environment, in `env -0` form.
        let shell = fake_shell(
            dir.path(),
            "shell",
            "export PATH=/fake/bin:$PATH EXTRA='two\nlines' PWD=/x SHLVL=3 TERM=dumb HUTAN_X=1\n\
             export ARGS=\"$*\" HAD_DAEMON_VAR=\"${HUTAN_LOGIN_ENV_LEAK:-no}\"\n\
             exec /usr/bin/env -0",
        );
        let env = capture(&shell, &base(), Duration::from_secs(10)).expect("capture");
        assert_eq!(env["PATH"], "/fake/bin:/usr/bin:/bin");
        assert_eq!(env["EXTRA"], "two\nlines");
        assert_eq!(env["ARGS"], "-l -c /usr/bin/env -0");
        assert_eq!(env["USER"], "ada", "the base is what the shell starts from");
        assert_eq!(env["HAD_DAEMON_VAR"], "no");
        for dropped in ["PWD", "SHLVL", "TERM", "HUTAN_X", "_"] {
            assert!(!env.contains_key(dropped), "{dropped}: {env:?}");
        }
    }

    #[test]
    fn a_shell_that_sleeps_is_killed_at_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survivor");
        let shell = fake_shell(
            dir.path(),
            "slow",
            &format!(
                // (Run without arguments, to settle the script, it does nothing.)
                "[ \"$1\" = -l ] || exit 0\n(sleep 3; touch {}) &\nsleep 30\nexec /usr/bin/env -0",
                marker.display()
            ),
        );
        let started = Instant::now();
        let error = capture(&shell, &base(), Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5), "returned late");
        // The background job of the shell went down with it.
        std::thread::sleep(Duration::from_millis(3500));
        assert!(!marker.exists(), "the process group was not killed");
    }

    #[test]
    fn failures_name_the_reason_and_never_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let fails = fake_shell(
            dir.path(),
            "fails",
            "echo SECRET=sentinel-value\necho sentinel-value >&2\nexit 3",
        );
        let error = capture(&fails, &base(), Duration::from_secs(10)).unwrap_err();
        assert!(
            error.contains("exited") && !error.contains("sentinel"),
            "{error}"
        );

        let garbage = fake_shell(dir.path(), "garbage", "echo 'not an environment'");
        let error = capture(&garbage, &base(), Duration::from_secs(10)).unwrap_err();
        assert_eq!(error, "the shell printed no environment");

        let error = capture("/nonexistent/shell", &base(), Duration::from_secs(10)).unwrap_err();
        assert!(error.contains("cannot start"), "{error}");
    }

    #[test]
    fn the_cache_waits_for_the_first_capture_and_then_answers_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let shell = fake_shell(
            dir.path(),
            "counting",
            &format!(
                "echo x >> {}\nexport RUNS=$(wc -l < {} | tr -d ' ')\nexec /usr/bin/env -0",
                counter.display(),
                counter.display()
            ),
        );
        // Creating the shell ran it once to settle it; count from zero.
        std::fs::remove_file(&counter).unwrap();
        let cache = LoginEnv::with_limits(
            Duration::from_secs(10),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(10),
        );
        assert_eq!(cache.get(&shell, &base()).unwrap()["RUNS"], "1");
        let started = Instant::now();
        assert_eq!(cache.get(&shell, &base()).unwrap()["RUNS"], "1", "cached");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_stale_capture_is_served_while_it_refreshes_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let shell = fake_shell(
            dir.path(),
            "counting",
            &format!(
                "echo x >> {}\nexport RUNS=$(wc -l < {} | tr -d ' ')\nexec /usr/bin/env -0",
                counter.display(),
                counter.display()
            ),
        );
        // Creating the shell ran it once to settle it; count from zero.
        std::fs::remove_file(&counter).unwrap();
        let cache = LoginEnv::with_limits(
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_secs(10),
        );
        assert_eq!(cache.get(&shell, &base()).unwrap()["RUNS"], "1");
        // Stale at once: this call answers with the capture it has, a refresh runs behind it.
        assert_eq!(cache.get(&shell, &base()).unwrap()["RUNS"], "1");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if cache.get(&shell, &base()).unwrap()["RUNS"] != "1" {
                break;
            }
            assert!(Instant::now() < deadline, "never refreshed");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_first_spawn_gives_up_waiting_and_later_ones_get_the_capture() {
        let dir = tempfile::tempdir().unwrap();
        let shell = fake_shell(
            dir.path(),
            "slowish",
            "sleep 1\nexport READY=yes\nexec /usr/bin/env -0",
        );
        let cache = LoginEnv::with_limits(
            Duration::from_secs(10),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_millis(100),
        );
        let started = Instant::now();
        assert_eq!(cache.get(&shell, &base()), None, "base environment only");
        assert!(started.elapsed() < Duration::from_millis(900));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(env) = cache.get(&shell, &base()) {
                assert_eq!(env["READY"], "yes");
                break;
            }
            assert!(Instant::now() < deadline, "never captured");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_shell_that_never_worked_is_retried_after_the_failure_interval_and_the_spawn_waits_for_it()
    {
        let dir = tempfile::tempdir().unwrap();
        let switch = dir.path().join("works-now");
        let shell = fake_shell(
            dir.path(),
            "recovering",
            &format!(
                "[ -e {} ] || exit 1\nexport RECOVERED=yes\nsleep 0.3\nexec /usr/bin/env -0",
                switch.display()
            ),
        );
        let cache = LoginEnv::with_limits(
            Duration::from_secs(10),
            Duration::from_secs(3600),
            Duration::from_millis(300),
            Duration::from_secs(10),
        );
        assert_eq!(cache.get(&shell, &base()), None, "the first capture fails");
        std::fs::write(&switch, "").unwrap();
        // Not due yet: no retry, no waiting.
        let started = Instant::now();
        assert_eq!(cache.get(&shell, &base()), None);
        assert!(started.elapsed() < Duration::from_millis(250));
        std::thread::sleep(Duration::from_millis(300));
        // Due: the retry starts and this spawn waits for it.
        assert_eq!(cache.get(&shell, &base()).unwrap()["RECOVERED"], "yes");
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_capture() {
        let dir = tempfile::tempdir().unwrap();
        let switch = dir.path().join("fail");
        let shell = fake_shell(
            dir.path(),
            "flaky",
            &format!(
                "[ -e {} ] && exit 1\nexport GOOD=yes\nexec /usr/bin/env -0",
                switch.display()
            ),
        );
        let cache = LoginEnv::with_limits(
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_secs(10),
        );
        assert_eq!(cache.get(&shell, &base()).unwrap()["GOOD"], "yes");
        std::fs::write(&switch, "").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        for _ in 0..3 {
            assert_eq!(cache.get(&shell, &base()).unwrap()["GOOD"], "yes");
            std::thread::sleep(Duration::from_millis(100));
        }
        // A shell that never worked has nothing to serve.
        let broken = fake_shell(dir.path(), "broken", "exit 1");
        assert_eq!(cache.get(&broken, &base()), None);
    }
}
