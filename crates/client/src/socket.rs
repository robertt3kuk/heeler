use std::ffi::OsString;
use std::path::PathBuf;

/// Where the daemon listens, mirroring the daemon's own resolution (architecture.md § Paths):
/// `STAFFY_SOCKET`, else `$STAFFY_HOME/staffyd.sock`, else
/// `~/Library/Application Support/Staffy/staffyd.sock`.
///
/// The client may not depend on the daemon crate, so the rule is repeated here.
pub fn default_socket_path() -> Option<PathBuf> {
    resolve(
        std::env::var_os("STAFFY_SOCKET"),
        std::env::var_os("STAFFY_HOME"),
        std::env::home_dir(),
    )
}

fn resolve(
    socket: Option<OsString>,
    home: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(socket) = socket.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(socket));
    }
    if let Some(home) = home.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("staffyd.sock"));
    }
    Some(user_home?.join("Library/Application Support/Staffy/staffyd.sock"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn socket_override_wins() {
        assert_eq!(
            resolve(os("/tmp/s.sock"), os("/tmp/hh"), Some("/Users/u".into())).as_deref(),
            Some(Path::new("/tmp/s.sock"))
        );
    }

    #[test]
    fn home_override_next() {
        assert_eq!(
            resolve(None, os("/tmp/hh"), Some("/Users/u".into())).as_deref(),
            Some(Path::new("/tmp/hh/staffyd.sock"))
        );
    }

    #[test]
    fn default_location() {
        assert_eq!(
            resolve(os(""), os(""), Some("/Users/u".into())).as_deref(),
            Some(Path::new(
                "/Users/u/Library/Application Support/Staffy/staffyd.sock"
            ))
        );
    }

    #[test]
    fn nothing_known() {
        assert_eq!(resolve(None, None, None), None);
    }
}
