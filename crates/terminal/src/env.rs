//! The environment a session's child process starts with.
//!
//! Terminal.app, iTerm2 and Ghostty start every tab from the user's login environment, not from
//! whatever the app inherited. Hutan does the same by default ([`EnvInherit::Login`]): the child
//! starts from an empty environment plus a few facts about the account and the system
//! ([`SystemFacts`]), and its login shell (`-l`) builds the rest from the user's startup files.
//! [`EnvInherit::Daemon`] keeps the older behavior: the child inherits the daemon's environment.
//!
//! The base is assembled by pure functions over [`SystemFacts`], so the rules are testable with
//! injected facts; [`system_facts`] gathers the real ones (account database, `confstr`,
//! `path_helper`, `defaults`, `launchctl`) and caches what cannot change while the daemon runs.
//! Nothing here evaluates shell output: `path_helper` is parsed, never `eval`'d.
//!
//! Precedence in the child, lowest to highest: this base < the session's `env` from the client
//! < `TERM`, `COLORTERM`, `TERM_PROGRAM` and the shell-integration variables < the
//! daemon-managed `HUTAN_*` variables (which the daemon puts into the session's `env` over
//! whatever the client sent).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};

use serde::Deserialize;

use crate::login_env::LoginEnv;
use crate::sys;

/// Where the child's environment comes from (`daemon.toml [terminal] env_inherit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvInherit {
    /// An empty environment plus the account's and system's facts ([`base_env`]); the login
    /// shell builds the rest.
    #[default]
    Login,
    /// The daemon's own environment, whoever started the daemon.
    Daemon,
}

/// `PATH` when `path_helper` is unavailable or prints nothing usable.
pub const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// `LANG` when neither the daemon nor the user's locale names a usable one.
pub const FALLBACK_LANG: &str = "en_US.UTF-8";

const SSH_AUTH_SOCK: &str = "SSH_AUTH_SOCK";
const PATH_HELPER: &str = "/usr/libexec/path_helper";
const LOCALE_DIR: &str = "/usr/share/locale";

/// What the login-environment base is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// `HOME` (account database).
    pub home: Option<String>,
    /// `USER` and `LOGNAME` (account database).
    pub user: Option<String>,
    /// `SHELL`: the account's login shell.
    pub shell: Option<String>,
    /// `TMPDIR`: the per-user temporary directory.
    pub temp_dir: Option<String>,
    /// `LANG`, `LC_*` and `__CF_USER_TEXT_ENCODING` as the daemon has them (only those that
    /// are set).
    pub daemon_locale: BTreeMap<String, String>,
    /// `LANG` derived from the user's locale preference; used only when the daemon has no
    /// `LANG`.
    pub user_lang: Option<String>,
    /// The output of `/usr/libexec/path_helper -s`.
    pub path_helper: Option<String>,
    /// The launchd user session's `SSH_AUTH_SOCK`.
    pub ssh_auth_sock: Option<String>,
}

/// The environment a login-mode child starts from: nothing but what [`SystemFacts`] provides.
pub fn base_env(facts: &SystemFacts) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
            env.insert(key.to_owned(), value.clone());
        }
    };
    put("HOME", &facts.home);
    put("USER", &facts.user);
    put("LOGNAME", &facts.user);
    put("SHELL", &facts.shell);
    put("TMPDIR", &facts.temp_dir);
    put(SSH_AUTH_SOCK, &facts.ssh_auth_sock);
    for (key, value) in &facts.daemon_locale {
        if is_locale_variable(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    if !env.contains_key("LANG") {
        env.insert(
            "LANG".to_owned(),
            facts
                .user_lang
                .clone()
                .unwrap_or_else(|| FALLBACK_LANG.to_owned()),
        );
    }
    let path = facts
        .path_helper
        .as_deref()
        .and_then(parse_path_helper)
        .unwrap_or_else(|| FALLBACK_PATH.to_owned());
    env.insert("PATH".to_owned(), path);
    env
}

/// `LANG`, `LC_*` and the macOS text-encoding hint: what is copied from the daemon when set.
fn is_locale_variable(name: &str) -> bool {
    name == "LANG" || name.starts_with("LC_") || name == "__CF_USER_TEXT_ENCODING"
}

/// The value of `PATH` in `path_helper -s` (`PATH="…"; export PATH;`) or `-c`
/// (`setenv PATH "…";`) output. The text is parsed, never evaluated; `None` when there is no
/// non-empty `PATH`.
pub fn parse_path_helper(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        let rest = line
            .strip_prefix("PATH=")
            .or_else(|| line.strip_prefix("setenv PATH "));
        let Some(rest) = rest else {
            continue;
        };
        let value = unquote(rest.trim_start());
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// The leading shell word of `text` without its quotes; stops at the closing quote (or at the
/// first `;` or space when unquoted), so trailing `; export PATH;` is ignored.
fn unquote(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(quote @ ('"' | '\'')) => chars.take_while(|c| *c != quote).collect(),
        Some(first) => std::iter::once(first)
            .chain(chars)
            .take_while(|c| *c != ';' && !c.is_whitespace())
            .collect(),
        None => String::new(),
    }
}

/// `LANG` for the macOS locale identifier `apple` (`defaults read -g AppleLocale`, e.g. `de_AT`
/// or `zh-Hans_CN@calendar=chinese`): `<language>_<REGION>.UTF-8`, using the first candidate
/// for which `exists` (a locale name like `de_AT.UTF-8`) holds, then the language with `_US`.
/// `None` when the identifier is not usable or no candidate exists.
pub fn lang_from_apple_locale(apple: &str, exists: impl Fn(&str) -> bool) -> Option<String> {
    let identifier = apple.trim().split('@').next()?;
    let (language, region) = match identifier.split_once('_') {
        Some((language, region)) => (language, Some(region)),
        None => (identifier, None),
    };
    let language = language.split('-').next()?;
    let language_ok =
        (2..=3).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase());
    if !language_ok {
        return None;
    }
    let region_ok =
        |region: &&str| region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase());
    let mut candidates = Vec::new();
    if let Some(region) = region.filter(region_ok) {
        candidates.push(format!("{language}_{region}.UTF-8"));
    }
    candidates.push(format!("{language}_US.UTF-8"));
    candidates.into_iter().find(|name| exists(name))
}

// ---- gathering the real facts ------------------------------------------------------------------

/// What does not change while the daemon runs.
#[derive(Debug)]
struct Stable {
    home: Option<String>,
    user: Option<String>,
    shell: Option<String>,
    temp_dir: Option<String>,
    daemon_locale: BTreeMap<String, String>,
    user_lang: Option<String>,
    path_helper: Option<String>,
}

static STABLE: LazyLock<Stable> = LazyLock::new(gather);
/// `launchctl getenv SSH_AUTH_SOCK`: `None` until first asked, then the answer.
static SSH_AUTH: Mutex<Option<Option<String>>> = Mutex::new(None);

/// Runs a system tool from an empty environment (plus the account's `HOME`/`USER`, which
/// `defaults` needs) and returns its trimmed stdout when it succeeds and prints something.
fn run(program: &str, args: &[&str], account: &Option<sys::Account>) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(account) = account {
        command
            .env("HOME", &account.home)
            .env("USER", &account.name);
    }
    let output = command.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn launchd_ssh_auth_sock(account: &Option<sys::Account>) -> Option<String> {
    run("/bin/launchctl", &["getenv", SSH_AUTH_SOCK], account)
}

fn gather() -> Stable {
    let account = sys::account();
    let daemon_env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let daemon_locale = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, value)| is_locale_variable(key) && !value.is_empty())
        .collect();
    let user_lang = run(
        "/usr/bin/defaults",
        &["read", "-g", "AppleLocale"],
        &account,
    )
    .and_then(|apple| {
        lang_from_apple_locale(&apple, |name| Path::new(LOCALE_DIR).join(name).is_dir())
    });
    Stable {
        home: account
            .as_ref()
            .map(|a| a.home.clone())
            .or_else(|| daemon_env("HOME")),
        user: account
            .as_ref()
            .map(|a| a.name.clone())
            .or_else(|| daemon_env("USER")),
        shell: account.as_ref().and_then(|a| a.shell.clone()),
        temp_dir: sys::user_temp_dir(),
        daemon_locale,
        user_lang,
        path_helper: run(PATH_HELPER, &["-s"], &account),
    }
}

/// The system facts of this machine and user. The first call gathers them (a few short
/// subprocesses; [`warm_up`] does that ahead of time) and later calls reuse them; only
/// `SSH_AUTH_SOCK` is asked again, when the socket it named has vanished.
pub fn system_facts() -> SystemFacts {
    let stable = &*STABLE;
    let ssh_auth_sock = {
        let mut cached = SSH_AUTH.lock().unwrap_or_else(|e| e.into_inner());
        let current = match cached.take() {
            None => launchd_ssh_auth_sock(&sys::account()),
            Some(Some(path)) if !Path::new(&path).exists() => {
                launchd_ssh_auth_sock(&sys::account())
            }
            Some(known) => known,
        };
        *cached = Some(current.clone());
        current
    };
    SystemFacts {
        home: stable.home.clone(),
        user: stable.user.clone(),
        shell: stable.shell.clone(),
        temp_dir: stable.temp_dir.clone(),
        daemon_locale: stable.daemon_locale.clone(),
        user_lang: stable.user_lang.clone(),
        path_helper: stable.path_helper.clone(),
        ssh_auth_sock,
    }
}

/// The login environment of `shell` as its login shell builds it ([`crate::login_env`]), for a
/// session that runs an explicit command and so has no shell of its own to build one: the last
/// good capture (refreshed in the background), `None` when there is none yet or capturing
/// failed, in which case the session starts from the base environment alone.
pub fn login_env(shell: &str, facts: &SystemFacts) -> Option<BTreeMap<String, String>> {
    LOGIN_ENV.get(shell, &capture_base(shell, facts))
}

/// What the capturing shell starts from: the base, with `SHELL` naming the shell itself.
fn capture_base(shell: &str, facts: &SystemFacts) -> BTreeMap<String, String> {
    let mut base = base_env(facts);
    base.insert("SHELL".to_owned(), shell.to_owned());
    base
}

static LOGIN_ENV: LazyLock<LoginEnv> = LazyLock::new(LoginEnv::default);

/// Gathers [`system_facts`] and the login environment of `shell` (default: the account's login
/// shell) on a background thread so the first session does not wait for the subprocesses. Call
/// once when the daemon starts in login mode.
pub fn warm_up(shell: Option<String>) {
    let spawned = std::thread::Builder::new()
        .name("hutan-env-facts".into())
        .spawn(move || {
            let facts = system_facts();
            if let Some(shell) = shell.or_else(|| facts.shell.clone()) {
                LOGIN_ENV.request(&shell, &capture_base(&shell, &facts));
            }
        });
    if let Err(error) = spawned {
        tracing::debug!(%error, "could not warm up the session environment facts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> SystemFacts {
        SystemFacts {
            home: Some("/Users/ada".into()),
            user: Some("ada".into()),
            shell: Some("/bin/zsh".into()),
            temp_dir: Some("/var/folders/xx/T/".into()),
            daemon_locale: BTreeMap::new(),
            user_lang: Some("de_AT.UTF-8".into()),
            path_helper: Some("PATH=\"/usr/local/bin:/usr/bin:/bin\"; export PATH;\n".into()),
            ssh_auth_sock: Some("/private/tmp/com.apple.launchd.x/Listeners".into()),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_base_holds_exactly_the_account_and_system_facts() {
        assert_eq!(
            base_env(&facts()),
            map(&[
                ("HOME", "/Users/ada"),
                ("USER", "ada"),
                ("LOGNAME", "ada"),
                ("SHELL", "/bin/zsh"),
                ("TMPDIR", "/var/folders/xx/T/"),
                ("LANG", "de_AT.UTF-8"),
                ("PATH", "/usr/local/bin:/usr/bin:/bin"),
                (
                    "SSH_AUTH_SOCK",
                    "/private/tmp/com.apple.launchd.x/Listeners"
                ),
            ])
        );
    }

    #[test]
    fn locale_variables_of_the_daemon_win_and_are_copied_only_when_set() {
        let mut with_locale = facts();
        with_locale.daemon_locale = map(&[
            ("LANG", "fr_FR.UTF-8"),
            ("LC_COLLATE", "C"),
            ("__CF_USER_TEXT_ENCODING", "0x1F5:0x0:0x0"),
            ("LANGUAGE", "not a locale variable"),
            ("PWD", "/leak"),
        ]);
        let env = base_env(&with_locale);
        assert_eq!(
            env["LANG"], "fr_FR.UTF-8",
            "the daemon's LANG beats the user's locale"
        );
        assert_eq!(env["LC_COLLATE"], "C");
        assert_eq!(env["__CF_USER_TEXT_ENCODING"], "0x1F5:0x0:0x0");
        assert!(!env.contains_key("LANGUAGE") && !env.contains_key("PWD"));

        // Without a daemon LANG, the user's locale; without that, the fallback.
        let mut only_lc = facts();
        only_lc.daemon_locale = map(&[("LC_ALL", "C")]);
        let env = base_env(&only_lc);
        assert_eq!(
            (env["LANG"].as_str(), env["LC_ALL"].as_str()),
            ("de_AT.UTF-8", "C")
        );
        let mut nothing = facts();
        nothing.user_lang = None;
        assert_eq!(base_env(&nothing)["LANG"], FALLBACK_LANG);
        assert!(!base_env(&nothing).contains_key("LC_ALL"));
    }

    #[test]
    fn missing_facts_leave_their_variables_out_and_path_falls_back() {
        let env = base_env(&SystemFacts::default());
        assert_eq!(
            env,
            map(&[("LANG", FALLBACK_LANG), ("PATH", FALLBACK_PATH)])
        );
        let mut garbage = facts();
        garbage.path_helper = Some("nothing useful\n".into());
        garbage.ssh_auth_sock = Some(String::new());
        garbage.temp_dir = None;
        let env = base_env(&garbage);
        assert_eq!(env["PATH"], FALLBACK_PATH);
        assert!(!env.contains_key("SSH_AUTH_SOCK") && !env.contains_key("TMPDIR"));
    }

    #[test]
    fn path_helper_sh_output_is_parsed_not_evaluated() {
        let output = "PATH=\"/usr/local/bin:/usr/bin:/bin:/opt/my dir/bin\"; export PATH;\n\
                      MANPATH=\"/usr/share/man\"; export MANPATH;\n";
        assert_eq!(
            parse_path_helper(output).as_deref(),
            Some("/usr/local/bin:/usr/bin:/bin:/opt/my dir/bin")
        );
        // Nothing in the value is expanded or executed.
        assert_eq!(
            parse_path_helper("PATH=\"$(touch /nope):`id`:$HOME/bin\"; export PATH;").as_deref(),
            Some("$(touch /nope):`id`:$HOME/bin")
        );
        assert_eq!(
            parse_path_helper("PATH='/a:/b'; export PATH;").as_deref(),
            Some("/a:/b")
        );
        assert_eq!(
            parse_path_helper("PATH=/a:/b; export PATH;").as_deref(),
            Some("/a:/b")
        );
    }

    #[test]
    fn path_helper_setenv_output_is_parsed() {
        assert_eq!(
            parse_path_helper("setenv PATH \"/usr/local/bin:/usr/bin\" ;\nsetenv MANPATH \"/m\" ;")
                .as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
        assert_eq!(
            parse_path_helper("setenv MANPATH \"/m\";\nsetenv PATH \"/a\";").as_deref(),
            Some("/a"),
            "the PATH line is found wherever it is"
        );
    }

    #[test]
    fn path_helper_output_without_a_path_is_rejected() {
        for output in [
            "",
            "\n",
            "PATH=\"\"; export PATH;",
            "MANPATH=\"/m\"; export MANPATH;",
            "PATH=",
            "setenv PATH",
        ] {
            assert_eq!(parse_path_helper(output), None, "{output:?}");
        }
    }

    #[test]
    fn apple_locales_become_utf8_lang_values() {
        let all = |_: &str| true;
        assert_eq!(
            lang_from_apple_locale("en_US", all).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(
            lang_from_apple_locale("de_AT\n", all).as_deref(),
            Some("de_AT.UTF-8")
        );
        assert_eq!(
            lang_from_apple_locale("en_US@rg=gbzzzz", all).as_deref(),
            Some("en_US.UTF-8"),
            "keywords after @ are dropped"
        );
        assert_eq!(
            lang_from_apple_locale("zh-Hans_CN", all).as_deref(),
            Some("zh_CN.UTF-8"),
            "the script subtag is dropped"
        );
        assert_eq!(
            lang_from_apple_locale("fr", all).as_deref(),
            Some("fr_US.UTF-8")
        );
        for bad in ["", "EN_us", "x", "english_US", "_US"] {
            assert_eq!(lang_from_apple_locale(bad, all), None, "{bad:?}");
        }
    }

    #[test]
    fn a_locale_the_system_does_not_have_falls_back_to_the_languages_us_variant() {
        let installed = |name: &str| name == "en_US.UTF-8" || name == "kk_KZ.UTF-8";
        assert_eq!(
            lang_from_apple_locale("en_KZ", installed).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(
            lang_from_apple_locale("kk_KZ", installed).as_deref(),
            Some("kk_KZ.UTF-8")
        );
        assert_eq!(lang_from_apple_locale("de_DE", installed), None);
    }
}
