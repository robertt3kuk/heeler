//! `heeler search` and `heeler index` against a real daemon whose history sources are temporary
//! directories (never the user's agent directories).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Out, TestDaemon, WAIT};
use heeler_daemon::{DaemonConfig, Paths};
use serde_json::Value;
use tokio::time::sleep;

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    t: TestDaemon,
}

async fn start(enabled: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    let claude = base.join("claude");
    let (codex, omp) = (base.join("codex"), base.join("omp"));
    for dir in [&home, &claude, &codex, &omp] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        home.join("daemon.toml"),
        format!(
            "[history]\nenabled = {enabled}\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
            claude.display(),
            codex.display(),
            omp.display()
        ),
    )
    .unwrap();
    let paths = Paths::in_home(&home);
    let daemon = heeler_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    Env {
        _tmp: tmp,
        claude,
        t: TestDaemon {
            _dir: tempfile::tempdir().unwrap(),
            paths,
            daemon,
        },
    }
}

fn append(path: &Path, lines: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

const RECENT: &str = r#"{"type":"user","cwd":"/work/app","message":{"role":"user","content":[{"type":"text","text":"why does the quokka scheduler stall under load"}]}}"#;
const OLD: &str = r#"{"type":"user","timestamp":"2020-01-01T00:00:00.000Z","cwd":"/work/old","message":{"role":"user","content":[{"type":"text","text":"ancient quokka question"}]}}"#;

/// Runs `heeler search …` until it prints at least `count` hits as JSON.
async fn search_until(env: &Env, args: &[&str], count: usize) -> Value {
    let start = Instant::now();
    loop {
        let mut full = vec!["search", "--json"];
        full.extend_from_slice(args);
        let out = env.t.heeler(&full).await;
        if out.code == 0 {
            let json = out.json();
            if json["hits"].as_array().unwrap().len() >= count {
                return json;
            }
        }
        assert!(
            start.elapsed() < WAIT,
            "no {count} hit(s) for {args:?}: {out:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_prints_json_and_human_output_and_honours_filters() {
    let env = start(true).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);
    append(&env.claude.join("-work-old/sess-b.jsonl"), &[OLD]);

    let json = search_until(&env, &["quokka"], 2).await;
    let hits = json["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let recent = hits.iter().find(|h| h["session_key"] == "sess-a").unwrap();
    assert_eq!(recent["source"], "claude");
    assert_eq!(recent["role"], "user");
    assert_eq!(recent["cwd"], "/work/app");
    assert_eq!(
        recent["resume_argv"],
        serde_json::json!(["claude", "--resume", "sess-a"])
    );
    assert!(recent["snippet"].as_str().unwrap().contains("«quokka»"));

    let human = env.t.heeler(&["search", "quokka", "scheduler"]).await.ok();
    assert!(human.stdout.contains("claude"), "{human:?}");
    assert!(human.stdout.contains("/work/app"), "{human:?}");
    assert!(
        human.stdout.contains("resume: claude --resume sess-a"),
        "{human:?}"
    );
    assert!(
        human.stdout.contains("«quokka»"),
        "no color off a terminal: {human:?}"
    );
    assert!(!human.stdout.contains('\u{1b}'), "{human:?}");

    // --since drops the old session, --source narrows, --limit caps.
    let recent_only = search_until(&env, &["quokka", "--since", "7d"], 1).await;
    assert_eq!(recent_only["hits"].as_array().unwrap().len(), 1);
    assert_eq!(recent_only["hits"][0]["session_key"], "sess-a");
    let other = env
        .t
        .heeler(&["search", "--json", "quokka", "--source", "codex"])
        .await
        .ok();
    assert!(other.json()["hits"].as_array().unwrap().is_empty());
    let capped = env
        .t
        .heeler(&["search", "--json", "quokka", "--limit", "1"])
        .await
        .ok();
    assert_eq!(capped.json()["hits"].as_array().unwrap().len(), 1);
    let none = env.t.heeler(&["search", "zzzzqqqq"]).await.ok();
    assert_eq!(none.stdout.trim(), "no matches");

    // Usage errors exit 2.
    for bad in [
        &["search", "--since", "yesterday", "x"][..],
        &["search", "--limit", "0", "x"],
        &["search", "--source", "opencode", "x"],
        &["search"],
    ] {
        assert_eq!(env.t.heeler(bad).await.code, 2, "{bad:?}");
    }
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn index_commands_enable_disable_status_clear_rebuild() {
    let env = start(false).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);

    // Disabled: status says so, search is refused with a pointer to `index enable`.
    let status = env
        .t
        .heeler(&["index", "status", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(status["enabled"], false);
    let human = env.t.heeler(&["index", "status"]).await.ok();
    assert!(human.stdout.contains("disabled"), "{human:?}");
    let refused: Out = env.t.heeler(&["search", "quokka"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("heeler index enable"),
        "{refused:?}"
    );

    let enabled = env
        .t
        .heeler(&["index", "enable", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(enabled["enabled"], true);
    search_until(&env, &["quokka"], 1).await;
    let human = env.t.heeler(&["index", "status"]).await.ok();
    assert!(human.stdout.contains("enabled"), "{human:?}");
    assert!(human.stdout.contains("claude"), "{human:?}");
    assert!(human.stdout.contains("MiB"), "{human:?}");
    let status = env
        .t
        .heeler(&["index", "status", "--json"])
        .await
        .ok()
        .json();
    let claude = status["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(
        (claude["files"].as_u64(), claude["rows"].as_u64()),
        (Some(1), Some(1))
    );

    let cleared = env
        .t
        .heeler(&["index", "clear", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(cleared["cleared"], true);
    let after = env
        .t
        .heeler(&["search", "--json", "quokka"])
        .await
        .ok()
        .json();
    assert!(after["hits"].as_array().unwrap().is_empty());

    let rebuilt = env
        .t
        .heeler(&["index", "rebuild", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(rebuilt["rebuilding"], true);
    search_until(&env, &["quokka"], 1).await;

    let disabled = env
        .t
        .heeler(&["index", "disable", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(disabled["enabled"], false);
    assert_eq!(env.t.heeler(&["search", "quokka"]).await.code, 1);
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_hit_that_cannot_resume_fails_cleanly() {
    let env = start(true).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);
    search_until(&env, &["quokka"], 1).await;
    let out = env.t.heeler(&["search", "quokka", "--resume", "5"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no hit 5"), "{out:?}");
    env.t.stop().await;
}
