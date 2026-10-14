//! `heeler ws open|ls|move|pin|unpin|recent|use` and `HEELER_WORKSPACE` for `heeler new` (roadmap
//! M12-A), through the real binary against a real in-process daemon. Every folder is a
//! temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tokio::time::sleep;

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn mkdir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    path.to_owned()
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

async fn ws_new(t: &TestDaemon, name: &str) -> String {
    t.heeler(&["ws", "new", name, "--json"]).await.ok().json()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Names of `heeler ws ls --json <args>`, in listing order.
async fn listed(t: &TestDaemon, args: &[&str]) -> Vec<String> {
    let mut full = vec!["ws", "ls", "--json"];
    full.extend_from_slice(args);
    let out = t.heeler(&full).await.ok().json();
    out["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_open_creates_a_workspace_once_and_finds_it_again() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let project = mkdir(&base.join("plain"));
    let inner = mkdir(&project.join("src/deep"));

    let created = t
        .heeler(&["ws", "open", s(&project), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(created["created"], true);
    let ws = &created["workspace"];
    assert_eq!(ws["name"], "plain");
    assert_eq!(ws["roots"], json!([s(&project)]));
    assert_eq!(ws["kind"], "project");
    assert_eq!(ws["created_by"], "cli");
    let id = ws["id"].as_str().unwrap().to_owned();

    // The human form says what happened.
    let human = t.heeler(&["ws", "open", s(&inner)]).await.ok();
    assert_eq!(human.stdout.trim(), format!("opened {id} (plain)"));
    let again = t
        .heeler(&["ws", "open", s(&inner), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(again["created"], false);
    assert_eq!(again["workspace"]["id"], id);

    // A relative path is resolved against the caller's directory, and a folder inside a git
    // repository opens the repository.
    let repo = mkdir(&base.join("clone"));
    mkdir(&repo.join(".git"));
    mkdir(&repo.join("crates/x"));
    let mut command = command(
        Path::new(HEELER),
        Some(&t.paths.socket),
        &["ws", "open", "crates/x", "--json"],
    );
    command.current_dir(&repo);
    let out = finish(command).await.ok().json();
    assert_eq!(out["created"], true);
    assert_eq!(out["workspace"]["roots"], json!([s(&repo)]));
    assert_eq!(out["workspace"]["name"], "clone");

    // Failures.
    let out = t.heeler(&["ws", "open", s(&base.join("missing"))]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.starts_with("heeler: not_found:"), "{out:?}");
    assert_eq!(t.heeler(&["ws", "open"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_hides_empty_workspaces_unless_pinned_or_home() {
    let t = TestDaemon::start().await;
    let busy = ws_new(&t, "busy").await;
    let _empty = ws_new(&t, "empty").await;
    let pinned = ws_new(&t, "pinned").await;
    t.new_session(&["--ws", &busy], &["cat"]).await;
    t.heeler(&["ws", "pin", &pinned]).await.ok();

    // Pinned first, then the manual order; Home is never hidden.
    assert_eq!(listed(&t, &[]).await, ["pinned", "Home", "busy"]);
    assert_eq!(
        listed(&t, &["--all"]).await,
        ["pinned", "Home", "busy", "empty"]
    );

    // The human table has the same rows; counts show live/total sessions.
    let human = t.heeler(&["ws", "ls"]).await.ok().stdout;
    let lines: Vec<_> = human.lines().collect();
    assert_eq!(lines.len(), 4, "{human}");
    assert!(
        lines[0].starts_with("ID") && lines[0].contains("SESSIONS"),
        "{human}"
    );
    assert!(
        lines[1].contains("pinned") && lines[1].contains("0/0"),
        "{human}"
    );
    assert!(
        lines[2].contains("Home") && lines[2].contains("home"),
        "{human}"
    );
    assert!(
        lines[3].contains("busy") && lines[3].contains("1/1"),
        "{human}"
    );
    assert!(!human.contains("empty"), "{human}");

    // Everything is hidden but Home on a fresh daemon with only empty workspaces.
    t.stop().await;
    let t = TestDaemon::start().await;
    ws_new(&t, "nothing").await;
    assert_eq!(listed(&t, &[]).await, ["Home"]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_sorts_by_manual_order_recent_activity_attention_and_name() {
    let t = TestDaemon::start().await;
    let zeta = ws_new(&t, "zeta").await;
    let alpha = ws_new(&t, "Alpha").await;
    let mid = ws_new(&t, "mid").await;
    for ws in [&zeta, &alpha, &mid] {
        t.new_session(&["--ws", ws], &["cat"]).await;
        sleep(Duration::from_millis(20)).await;
    }

    // Manual: Home first, then creation order.
    assert_eq!(
        listed(&t, &["--sort", "manual"]).await,
        ["Home", "zeta", "Alpha", "mid"]
    );
    assert_eq!(
        listed(&t, &[]).await,
        ["Home", "zeta", "Alpha", "mid"],
        "the default"
    );
    // Name, ignoring case.
    assert_eq!(
        listed(&t, &["--sort", "name"]).await,
        ["Alpha", "Home", "mid", "zeta"]
    );
    // Recent: the newest session's workspace first, Home (no sessions) last.
    assert_eq!(
        listed(&t, &["--sort", "recent"]).await,
        ["mid", "Alpha", "zeta", "Home"]
    );

    // Attention: a blocked agent in `zeta` puts it first although `mid` is more recent.
    let session = t
        .sessions()
        .await
        .into_iter()
        .find(|s| s["workspace_id"] == zeta)
        .unwrap();
    let report = |state: &str, seq: u64| {
        json!({
            "session_id": session["id"], "source": "test", "agent": "omp",
            "state": state, "seq": seq,
        })
        .to_string()
    };
    t.heeler(&["api", "agent.report", &report("blocked", 1)])
        .await
        .ok();
    assert_eq!(
        listed(&t, &["--sort", "attention"]).await,
        ["zeta", "mid", "Alpha", "Home"]
    );
    // Not blocked any more: back to the recent order.
    t.heeler(&["api", "agent.report", &report("working", 2)])
        .await
        .ok();
    assert_eq!(
        listed(&t, &["--sort", "attention"]).await,
        ["mid", "Alpha", "zeta", "Home"]
    );

    // Pinned workspaces come first in every mode.
    t.heeler(&["ws", "pin", "Alpha"]).await.ok();
    for mode in ["manual", "recent", "attention", "name"] {
        assert_eq!(listed(&t, &["--sort", mode]).await[0], "Alpha", "{mode}");
    }

    let out = t.heeler(&["ws", "ls", "--sort", "sideways"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_move_reorders_by_id_or_name() {
    let t = TestDaemon::start().await;
    let a = ws_new(&t, "a").await;
    let _b = ws_new(&t, "b").await;
    let _c = ws_new(&t, "c").await;
    assert_eq!(listed(&t, &["--all"]).await, ["Home", "a", "b", "c"]);

    let out = t
        .heeler(&["ws", "move", "c", "--before", &a, "--json"])
        .await
        .ok()
        .json();
    let order: Vec<_> = out["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        ["Home", "c", "a", "b"],
        "the result is the new order"
    );
    assert_eq!(listed(&t, &["--all"]).await, ["Home", "c", "a", "b"]);

    let human = t.heeler(&["ws", "move", "Home", "--last"]).await.ok();
    assert!(human.stdout.contains("to the end"), "{human:?}");
    assert_eq!(listed(&t, &["--all"]).await, ["c", "a", "b", "Home"]);
    let human = t.heeler(&["ws", "move", &a, "--before", "Home"]).await.ok();
    assert!(human.stdout.contains("before"), "{human:?}");
    assert_eq!(listed(&t, &["--all"]).await, ["c", "b", "a", "Home"]);

    // Exactly one of --before and --last.
    assert_eq!(t.heeler(&["ws", "move", "a"]).await.code, 2);
    assert_eq!(
        t.heeler(&["ws", "move", "a", "--last", "--before", "b"])
            .await
            .code,
        2
    );
    // Unknown targets fail without moving anything.
    let out = t.heeler(&["ws", "move", "nope", "--last"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no workspace matches"), "{out:?}");
    assert_eq!(
        t.heeler(&["ws", "move", "a", "--before", "nope"])
            .await
            .code,
        1
    );
    assert_eq!(listed(&t, &["--all"]).await, ["c", "b", "a", "Home"]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_pin_and_unpin() {
    let t = TestDaemon::start().await;
    let id = ws_new(&t, "later").await;
    let _other = ws_new(&t, "other").await;

    let pinned = t
        .heeler(&["ws", "pin", "later", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(pinned["pinned"], true);
    assert_eq!(pinned["id"], id);
    let human = t.heeler(&["ws", "pin", &id]).await.ok();
    assert_eq!(
        human.stdout.trim(),
        format!("pinned {id} (later)"),
        "idempotent"
    );
    assert_eq!(listed(&t, &["--all"]).await, ["later", "Home", "other"]);

    let unpinned = t.heeler(&["ws", "unpin", &id, "--json"]).await.ok().json();
    assert_eq!(unpinned["pinned"], false);
    let human = t.heeler(&["ws", "unpin", "later"]).await.ok();
    assert!(human.stdout.starts_with("unpinned "), "{human:?}");
    assert_eq!(listed(&t, &["--all"]).await, ["Home", "later", "other"]);

    assert_eq!(t.heeler(&["ws", "pin", "nope"]).await.code, 1);
    assert_eq!(t.heeler(&["ws", "pin"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_recent_lists_open_and_closed_projects_newest_first() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let one = mkdir(&base.join("one"));
    let two = mkdir(&base.join("two"));
    let id_one = t
        .heeler(&["ws", "open", s(&one), "--json"])
        .await
        .ok()
        .json()["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sleep(Duration::from_millis(10)).await;
    t.heeler(&["ws", "open", s(&two)]).await.ok();
    t.heeler(&["ws", "close", &id_one]).await.ok();

    let out = t.heeler(&["ws", "recent", "--json"]).await.ok().json();
    let projects = out["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2, "{out}");
    assert_eq!(projects[0]["name"], "two");
    assert!(projects[0]["workspace_id"].is_string(), "open");
    assert_eq!(projects[1]["name"], "one");
    assert_eq!(projects[1]["path"], s(&one));
    assert!(projects[1].get("workspace_id").is_none(), "closed");

    let human = t.heeler(&["ws", "recent"]).await.ok().stdout;
    let lines: Vec<_> = human.lines().collect();
    assert_eq!(lines.len(), 3, "{human}");
    assert!(
        lines[0].starts_with("NAME") && lines[0].contains("PATH"),
        "{human}"
    );
    assert!(
        lines[1].starts_with("two") && lines[1].ends_with(s(&two)),
        "{human}"
    );
    assert!(
        lines[2].starts_with("one") && lines[2].contains("closed"),
        "{human}"
    );

    let out = t
        .heeler(&["ws", "recent", "--limit", "1", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(out["projects"].as_array().unwrap().len(), 1);
    assert_eq!(t.heeler(&["ws", "recent", "--limit", "0"]).await.code, 2);

    // Nothing remembered yet on another daemon.
    t.stop().await;
    let t = TestDaemon::start().await;
    assert_eq!(
        t.heeler(&["ws", "recent"]).await.ok().stdout.trim(),
        "no recent projects"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_use_prints_an_export_and_new_honours_it() {
    let t = TestDaemon::start().await;
    let work = ws_new(&t, "work").await;
    let other = ws_new(&t, "other").await;

    let line = t.heeler(&["ws", "use", "work"]).await.ok().stdout;
    assert_eq!(line.trim(), format!("export HEELER_WORKSPACE={work}"));
    let json = t
        .heeler(&["ws", "use", &other[..20], "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["workspace_id"], other);
    assert_eq!(json["name"], "other");
    assert_eq!(json["export"], format!("export HEELER_WORKSPACE={other}"));
    let out = t.heeler(&["ws", "use", "nope"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.is_empty(), "nothing to eval: {out:?}");

    let with_env = |extra: &[&str], value: &str| {
        let mut args = vec!["new", "--json"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--", "cat"]);
        let mut command = command(Path::new(HEELER), Some(&t.paths.socket), &args);
        command.env("HEELER_WORKSPACE", value);
        finish(command)
    };
    // The variable chooses the workspace (by id or name); `--ws` wins over it; without either
    // the session goes to Home.
    let out = with_env(&[], &work).await.ok().json();
    assert_eq!(out["session"]["workspace_id"], work);
    let out = with_env(&[], "other").await.ok().json();
    assert_eq!(out["session"]["workspace_id"], other);
    let out = with_env(&["--ws", "work"], "other").await.ok().json();
    assert_eq!(out["session"]["workspace_id"], work);
    let out = with_env(&[], "").await.ok().json();
    let home = t.heeler(&["ws", "ls", "--json"]).await.ok().json()["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["kind"] == "home")
        .unwrap()["id"]
        .clone();
    assert_eq!(
        out["session"]["workspace_id"], home,
        "an empty variable is unset"
    );

    // A stale value says where it came from and creates nothing.
    let before = t.sessions().await.len();
    let out = with_env(&[], "gone").await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("no workspace matches `gone`"),
        "{out:?}"
    );
    assert!(out.stderr.contains("$HEELER_WORKSPACE"), "{out:?}");
    assert_eq!(t.sessions().await.len(), before);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_and_new_take_workspace_names() {
    let t = TestDaemon::start().await;
    let work = ws_new(&t, "work").await;
    let id = t.new_session(&["--ws", "work"], &["cat"]).await;
    t.new_session(&[], &["cat"]).await;
    let out = t
        .heeler(&["ls", "--ws", "work", "--json"])
        .await
        .ok()
        .json();
    let sessions = out["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], id);
    assert_eq!(sessions[0]["workspace_id"], work);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_close_keeps_home_and_the_rest_of_the_workspace_commands_work_together() {
    let t = TestDaemon::start().await;
    let home = t.heeler(&["ws", "ls", "--json"]).await.ok().json()["workspaces"][0].clone();
    let out = t
        .heeler(&["ws", "close", home["id"].as_str().unwrap()])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("invalid_state"), "{out:?}");
    let renamed = t
        .heeler(&[
            "ws",
            "rename",
            home["id"].as_str().unwrap(),
            "Notes",
            "--json",
        ])
        .await
        .ok()
        .json();
    assert_eq!(renamed["name"], "Notes");
    assert_eq!(renamed["kind"], "home");
    assert_eq!(listed(&t, &[]).await, ["Notes"]);
    // And Home resolves by its new name.
    let line = t.heeler(&["ws", "use", "notes"]).await.ok().stdout;
    assert!(line.contains(home["id"].as_str().unwrap()), "{line}");
    let _: Value = home;
    t.stop().await;
}
