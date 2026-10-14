//! `xtask bench`: measures the performance budgets of `docs/vision.md` (B1–B6) end to end.
//!
//! Every scenario runs against its own daemon in a temporary `HEELER_HOME`, with the real
//! release `heeler-desktop` (a window opens; nothing is captured). The desktop is driven by a
//! seeded `desktop-state.json` and, for the latency scenarios, by `HEELER_BENCH_INPUT`; its
//! `HEELER_TRACE_FILE` is the measurement channel (see `heeler_terminal_view::trace`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use heeler_client::{Client, ConnectOptions};
use heeler_core::TerminalId;
use heeler_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Request, SessionCreate, SessionInfo,
    SessionResult, SessionSpec, TerminalSpec,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

use crate::proc::{self, Guard};
use crate::seed;
use crate::stats::{median, percentile};

const MIB: f64 = 1024.0 * 1024.0;

/// A budget from `docs/vision.md` and the metric that decides it.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub id: &'static str,
    pub name: &'static str,
    pub metric: &'static str,
    pub unit: &'static str,
    /// Passes when the median is at most this.
    pub limit: f64,
}

pub const BUDGETS: &[Budget] = &[
    Budget {
        id: "B1",
        name: "Idle desktop RSS, 1 window, 4 terminals",
        metric: "b1_desktop_rss_mb",
        unit: "MB",
        limit: 150.0,
    },
    Budget {
        id: "B2",
        name: "Idle daemon RSS, 10 sessions x 10k lines",
        metric: "b2_daemon_rss_mb",
        unit: "MB",
        limit: 120.0,
    },
    Budget {
        id: "B3",
        name: "Idle CPU, daemon + desktop, 30 s",
        metric: "b3_idle_cpu_pct",
        unit: "%",
        limit: 0.5,
    },
    Budget {
        id: "B4",
        name: "Keystroke to glyph, p95 over 500 keys",
        metric: "b4_key_p95_ms",
        unit: "ms",
        limit: 16.0,
    },
    Budget {
        id: "B5",
        name: "Flood: p95 key latency in a second terminal",
        metric: "b5_key_p95_ms",
        unit: "ms",
        limit: 50.0,
    },
    Budget {
        id: "B5",
        name: "Flood: daemon RSS growth",
        metric: "b5_daemon_growth_mb",
        unit: "MB",
        limit: 50.0,
    },
    Budget {
        id: "B6",
        name: "Cold start to first terminal text painted",
        metric: "b6_cold_start_ms",
        unit: "ms",
        limit: 500.0,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// B1, B2, B3 in one run: 10 loaded sessions, 4 of them on screen, then idle.
    Idle,
    /// B4.
    Typing,
    /// B5.
    Flood,
    /// B6.
    ColdStart,
}

impl Scenario {
    const ALL: [Scenario; 4] = [
        Scenario::Idle,
        Scenario::Typing,
        Scenario::Flood,
        Scenario::ColdStart,
    ];

    fn name(self) -> &'static str {
        match self {
            Scenario::Idle => "idle (B1,B2,B3)",
            Scenario::Typing => "typing (B4)",
            Scenario::Flood => "flood (B5)",
            Scenario::ColdStart => "cold start (B6)",
        }
    }

    fn budgets(self) -> &'static [&'static str] {
        match self {
            Scenario::Idle => &["B1", "B2", "B3"],
            Scenario::Typing => &["B4"],
            Scenario::Flood => &["B5"],
            Scenario::ColdStart => &["B6"],
        }
    }
}

#[derive(Debug)]
pub struct Options {
    /// Budget ids to measure (`B1`…); empty means all.
    pub only: Vec<String>,
    pub json: bool,
    pub runs: usize,
    /// Profile the desktop and daemon with `sample(1)` during the latency scenarios.
    pub sample: bool,
    /// Milliseconds between keystrokes of the typing and flood scenarios (B4, B5); defaults
    /// [`TYPING_INTERVAL_MS`] and [`FLOOD_INTERVAL_MS`].
    pub key_interval_ms: Option<u64>,
}

type Metrics = BTreeMap<String, f64>;

pub async fn run(options: Options) -> Result<()> {
    let root = workspace_root();
    let wanted: Vec<String> = options.only.iter().map(|s| s.to_uppercase()).collect();
    for id in &wanted {
        if !BUDGETS.iter().any(|b| b.id == id) {
            bail!("unknown budget {id}; known: B1..B6");
        }
    }
    let selected = |s: &Scenario| {
        wanted.is_empty() || s.budgets().iter().any(|b| wanted.iter().any(|w| w == b))
    };
    let scenarios: Vec<Scenario> = Scenario::ALL.into_iter().filter(selected).collect();

    let mut bins = build(&root)?;
    if options.sample {
        bins.sample_dir = Some(root.join("target/bench"));
    }
    let machine = machine_info();
    let mut results: BTreeMap<&'static str, Vec<Metrics>> = BTreeMap::new();
    for scenario in scenarios {
        for run in 1..=options.runs {
            eprintln!("== {} run {run}/{}", scenario.name(), options.runs);
            let measured = tokio::select! {
                result = run_scenario(scenario, &bins, options.key_interval_ms) => result?,
                _ = tokio::signal::ctrl_c() => bail!("interrupted"),
            };
            results.entry(scenario.name()).or_default().push(measured);
        }
    }

    let rows = summarize(&results, &wanted);
    let report = json!({
        "timestamp": unix_secs(),
        "machine": machine,
        "runs": options.runs,
        "budgets": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
        "scenarios": results.iter().map(|(name, runs)| (name.to_string(), json!(runs))).collect::<serde_json::Map<_, _>>(),
    });
    let dir = root.join("target/bench");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", unix_secs()));
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_table(&rows);
    }
    eprintln!("wrote {}", path.display());
    if rows.iter().any(|r| !r.pass) {
        bail!("at least one budget missed");
    }
    Ok(())
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

struct Bins {
    desktop: PathBuf,
    daemon: PathBuf,
    cli: PathBuf,
    /// Where `--sample` writes `sample(1)` output; `None` without the flag.
    sample_dir: Option<PathBuf>,
}

fn build(root: &Path) -> Result<Bins> {
    eprintln!("building release binaries");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "heeler-desktop",
            "-p",
            "heeler-daemon",
            "-p",
            "heeler-cli",
        ])
        .current_dir(root)
        .status()
        .context("running cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), PathBuf::from)
        .join("release");
    let bins = Bins {
        desktop: target.join("heeler-desktop"),
        daemon: target.join("heelerd"),
        cli: target.join("heeler"),
        sample_dir: None,
    };
    for bin in [&bins.desktop, &bins.daemon, &bins.cli] {
        if !bin.is_file() {
            bail!("missing {}", bin.display());
        }
    }
    Ok(bins)
}

fn machine_info() -> Value {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    json!({
        "cpu": run("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "memory_bytes": run("sysctl", &["-n", "hw.memsize"]).parse::<u64>().unwrap_or(0),
        "os": format!("macOS {}", run("sw_vers", &["-productVersion"])),
        "rustc": run("rustc", &["--version"]),
    })
}

// ---- report ------------------------------------------------------------------------------

struct Row {
    budget: Budget,
    values: Vec<f64>,
    median: f64,
    pass: bool,
}

impl Row {
    fn to_json(&self) -> Value {
        json!({
            "id": self.budget.id,
            "name": self.budget.name,
            "metric": self.budget.metric,
            "unit": self.budget.unit,
            "limit": self.budget.limit,
            "values": self.values,
            "median": self.median,
            "pass": self.pass,
        })
    }
}

fn summarize(results: &BTreeMap<&'static str, Vec<Metrics>>, wanted: &[String]) -> Vec<Row> {
    BUDGETS
        .iter()
        .filter(|b| wanted.is_empty() || wanted.iter().any(|w| w == b.id))
        .filter_map(|budget| {
            let values: Vec<f64> = results
                .values()
                .flatten()
                .filter_map(|m| m.get(budget.metric).copied())
                .collect();
            let med = median(&values)?;
            Some(Row {
                budget: *budget,
                pass: med <= budget.limit,
                median: med,
                values,
            })
        })
        .collect()
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<3} {:<46} {:>8} {:>10}  {:<28} result",
        "id", "metric", "budget", "median", "runs"
    );
    for row in rows {
        let runs: Vec<String> = row.values.iter().map(|v| format!("{v:.2}")).collect();
        println!(
            "{:<3} {:<46} {:>8} {:>10}  {:<28} {}",
            row.budget.id,
            row.budget.name,
            format!("<= {} {}", row.budget.limit, row.budget.unit),
            format!("{:.2} {}", row.median, row.budget.unit),
            runs.join(", "),
            if row.pass { "PASS" } else { "FAIL" },
        );
    }
}

// ---- stack: one daemon (+ desktop) in a temporary home ----------------------------------

struct Stack<'a> {
    bins: &'a Bins,
    home: TempDir,
    client: Client,
    // Declared after `client` so the connection closes before the daemon is stopped.
    daemon: Guard,
    desktop: Option<Guard>,
}

impl<'a> Stack<'a> {
    async fn start(bins: &'a Bins) -> Result<Self> {
        let home = tempfile::Builder::new()
            .prefix("heeler-bench-")
            .tempdir()
            .context("creating the temporary HEELER_HOME")?;
        let log = std::fs::File::create(home.path().join("daemon.out"))?;
        let mut daemon = Guard::spawn(
            "heelerd",
            Command::new(&bins.daemon)
                .env_remove("HEELER_SOCKET")
                .env("HEELER_HOME", home.path())
                .env("HEELER_LOG", "warn")
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log),
        )?;
        let socket = home.path().join("heelerd.sock");
        let started = Instant::now();
        let client = loop {
            daemon.ensure_running()?;
            let attempt = Client::connect(ConnectOptions {
                socket: Some(socket.clone()),
                autostart: None,
                client: ClientInfo {
                    kind: ClientKind::Cli,
                    version: "xtask-bench".to_owned(),
                },
            })
            .await;
            match attempt {
                Ok(client) => break client,
                Err(err) if started.elapsed() > Duration::from_secs(15) => {
                    bail!("cannot reach the daemon: {err}")
                }
                Err(_) => sleep(Duration::from_millis(50)).await,
            }
        };
        Ok(Self {
            bins,
            home,
            client,
            daemon,
            desktop: None,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// With `--sample`: runs `sample <pid> <secs>` in the background, writing
    /// `target/bench/sample-<timestamp>-<name>.txt`. Wait for the returned child.
    fn sample(&self, pid: u32, secs: u32, name: &str) -> Option<std::process::Child> {
        let dir = self.bins.sample_dir.as_ref()?;
        let _ = std::fs::create_dir_all(dir);
        let file = dir.join(format!("sample-{}-{name}.txt", unix_secs()));
        eprintln!("sampling {name} ({pid}) for {secs}s -> {}", file.display());
        Command::new("sample")
            .arg(pid.to_string())
            .arg(secs.to_string())
            .arg("1")
            .arg("-file")
            .arg(file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    async fn create(&self, command: &[&str]) -> Result<SessionInfo> {
        let result: SessionResult = self
            .client
            .call(Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: command.iter().map(|s| (*s).to_owned()).collect(),
                    ..TerminalSpec::default()
                }),
            }))
            .await?;
        Ok(result.session)
    }

    async fn type_into(&self, session: &SessionInfo, text: &str) -> Result<()> {
        let terminal: TerminalId = session.terminal_id.context("session has no terminal")?;
        self.client.send_input(terminal, text.as_bytes()).await?;
        Ok(())
    }

    async fn clients(&self) -> Result<u32> {
        let status: DaemonStatusInfo = self.client.call(Request::DaemonStatus(Empty {})).await?;
        Ok(status.clients)
    }

    fn daemon_usage(&self) -> Result<proc::Usage> {
        proc::usage(self.daemon.pid())
    }

    /// Seeds the layout and starts the desktop. `trace` enables `HEELER_TRACE_LATENCY`;
    /// `script` is a key script for `HEELER_BENCH_INPUT`.
    fn launch_desktop(
        &mut self,
        sessions: &[&SessionInfo],
        trace: bool,
        script: Option<&Value>,
    ) -> Result<()> {
        let ids: Vec<String> = sessions.iter().map(|s| s.id.to_string()).collect();
        std::fs::write(
            self.path("desktop-state.json"),
            serde_json::to_string_pretty(&seed::layout(&ids)?)?,
        )?;
        let log = std::fs::File::create(self.path("desktop.out"))?;
        let mut command = Command::new(&self.bins.desktop);
        command
            .env_remove("HEELER_SOCKET")
            .env("HEELER_HOME", self.home.path())
            .env("RUST_LOG", "info")
            .env("RUST_BACKTRACE", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if trace {
            command
                .env("HEELER_TRACE_LATENCY", "1")
                .env("HEELER_TRACE_FILE", self.path("trace.jsonl"));
        }
        if let Some(script) = script {
            let path = self.path("keys.json");
            std::fs::write(&path, serde_json::to_string(script)?)?;
            command.env("HEELER_BENCH_INPUT", path);
        }
        self.desktop = Some(Guard::spawn("heeler-desktop", &mut command)?);
        Ok(())
    }

    fn desktop(&mut self) -> Result<&mut Guard> {
        self.desktop.as_mut().context("the desktop is not running")
    }

    /// Fails when the desktop exited or did not restore the seeded layout.
    fn check_desktop(&mut self) -> Result<()> {
        self.desktop()?.ensure_running()?;
        let log = std::fs::read_to_string(self.path("desktop.out")).unwrap_or_default();
        if log.contains("using the default layout") {
            bail!("the desktop ignored the seeded layout:\n{log}");
        }
        Ok(())
    }

    fn trace(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Waits for a trace event; the desktop must stay up.
    async fn wait_trace(
        &mut self,
        what: &str,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.trace().into_iter().find(&pred) {
                return Ok(event);
            }
            self.check_desktop()?;
            if Instant::now() > deadline {
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    /// What the daemon and the desktop printed, for failure reports.
    fn logs(&self) -> String {
        let tail = |name: &str| {
            let text = std::fs::read_to_string(self.path(name)).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        };
        format!(
            "--- daemon.out ---\n{}\n--- desktop.out ---\n{}\n--- trace.jsonl (tail) ---\n{}",
            tail("daemon.out"),
            tail("desktop.out"),
            tail("trace.jsonl")
        )
    }

    /// Quits the desktop with SIGTERM (it saves its layout and leaves the daemon running), then
    /// stops the daemon and its sessions through `heeler daemon stop --stop-sessions`, the way a
    /// user would. Fails when either had to be SIGKILLed, crashed, or left a process behind.
    fn teardown(mut self) -> Result<()> {
        let desktop = self
            .desktop
            .take()
            .map(|mut desktop| desktop.stop(Duration::from_secs(10)).check());
        self.daemon.note_descendants();
        let cli = Command::new(&self.bins.cli)
            .args(["daemon", "stop", "--stop-sessions"])
            .env_remove("HEELER_SOCKET")
            .env("HEELER_HOME", self.home.path())
            .stdin(Stdio::null())
            .output()
            .context("running `heeler daemon stop`")?;
        if !cli.status.success() {
            eprintln!(
                "heeler daemon stop failed ({}): {}",
                cli.status,
                String::from_utf8_lossy(&cli.stderr)
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.daemon.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let daemon = self.daemon.stop(Duration::from_secs(10)).check();
        desktop.transpose()?;
        daemon
    }
}

fn startup_us(events: &[Value], stage: &str) -> Option<u64> {
    events
        .iter()
        .find(|e| e["ev"] == "startup" && e["stage"] == stage)
        .and_then(|e| e["t_us"].as_u64())
}

fn is_event(event: &Value, name: &str) -> bool {
    event["ev"] == name
}

fn key_stage_ms(events: &[Value], field: &str, window: Option<(u64, u64)>) -> Vec<f64> {
    events
        .iter()
        .filter(|e| is_event(e, "key"))
        .filter(|e| {
            window.is_none_or(|(from, to)| e["t_us"].as_u64().is_some_and(|t| from <= t && t <= to))
        })
        .filter_map(|e| e[field].as_f64())
        .map(|us| us / 1000.0)
        .collect()
}

fn key_totals_ms(events: &[Value], window: Option<(u64, u64)>) -> Vec<f64> {
    key_stage_ms(events, "total_us", window)
}

/// Median and p95 of each stage of the keystroke path.
fn record_stages(
    metrics: &mut Metrics,
    prefix: &str,
    events: &[Value],
    window: Option<(u64, u64)>,
) {
    for (name, field) in [
        ("key_to_sent", "key_to_sent_us"),
        ("sent_to_echo", "sent_to_echo_us"),
        ("echo_to_paint", "echo_to_paint_us"),
        ("echo_to_prepaint", "echo_to_prepaint_us"),
    ] {
        let values = key_stage_ms(events, field, window);
        for (suffix, p) in [("p50", 50.0), ("p95", 95.0)] {
            if let Some(v) = percentile(&values, p) {
                metrics.insert(format!("{prefix}_{name}_{suffix}_ms"), v);
            }
        }
    }
}

fn record_latency(metrics: &mut Metrics, prefix: &str, totals: &[f64]) {
    for (suffix, p) in [("p50", 50.0), ("p95", 95.0), ("p99", 99.0), ("max", 100.0)] {
        if let Some(v) = percentile(totals, p) {
            metrics.insert(format!("{prefix}_key_{suffix}_ms"), v);
        }
    }
    metrics.insert(format!("{prefix}_keys_measured"), totals.len() as f64);
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

// ---- scenarios -----------------------------------------------------------------------------

async fn run_scenario(
    scenario: Scenario,
    bins: &Bins,
    key_interval_ms: Option<u64>,
) -> Result<Metrics> {
    let mut stack = Stack::start(bins).await?;
    let result = match scenario {
        Scenario::Idle => idle(&mut stack).await,
        Scenario::Typing => typing(&mut stack, key_interval_ms).await,
        Scenario::Flood => flood(&mut stack, key_interval_ms).await,
        Scenario::ColdStart => cold_start(&mut stack).await,
    };
    let result = result.map_err(|err| err.context(stack.logs()));
    let teardown = stack.teardown();
    let metrics = result?;
    teardown?;
    Ok(metrics)
}

/// `sh` session that prints a prompt and then echoes what is typed, like a shell at rest.
const PROMPT_CAT: &[&str] = &["sh", "-c", "printf 'bench> '; exec cat"];

/// Seconds of idling before B1 is sampled, and the B3 window after it.
const SETTLE: Duration = Duration::from_secs(10);
const CPU_WINDOW: Duration = Duration::from_secs(30);
const LOADED_SESSIONS: usize = 10;
const SHOWN_SESSIONS: usize = 4;

async fn idle(stack: &mut Stack<'_>) -> Result<Metrics> {
    let mut metrics = Metrics::new();
    // 12 000 colored lines overflow the 10 000-line scrollback; the file marks "printed".
    let mut sessions = Vec::new();
    for n in 0..LOADED_SESSIONS {
        let done = stack.path(&format!("done-{n}"));
        let script = format!(
            "awk 'BEGIN{{for(i=1;i<=12000;i++) printf \"\\033[3%dmline %d\\033[0m the quick brown fox jumps over the lazy dog 0123456789\\n\", i%7+1, i}}'; \
             touch '{}'; exec cat",
            done.display()
        );
        sessions.push((stack.create(&["sh", "-c", &script]).await?, done));
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while sessions.iter().any(|(_, done)| !done.exists()) {
        if Instant::now() > deadline {
            bail!("sessions did not finish printing");
        }
        sleep(Duration::from_millis(100)).await;
    }
    sleep(Duration::from_secs(2)).await;
    let detached = stack.daemon_usage()?;
    metrics.insert(
        "b2_daemon_rss_detached_mb".into(),
        detached.rss_bytes as f64 / MIB,
    );

    let shown: Vec<&SessionInfo> = sessions
        .iter()
        .take(SHOWN_SESSIONS)
        .map(|(s, _)| s)
        .collect();
    stack.launch_desktop(&shown, false, None)?;
    let started = Instant::now();
    while stack.clients().await? < 2 {
        stack.check_desktop()?;
        if started.elapsed() > Duration::from_secs(30) {
            bail!("the desktop never connected");
        }
        sleep(Duration::from_millis(100)).await;
    }
    sleep(SETTLE).await;
    stack.check_desktop()?;

    let desktop_pid = stack.desktop()?.pid();
    let desktop0 = proc::usage(desktop_pid)?;
    let daemon0 = stack.daemon_usage()?;
    metrics.insert("b1_desktop_rss_mb".into(), desktop0.rss_bytes as f64 / MIB);
    metrics.insert(
        "b1_desktop_footprint_mb".into(),
        desktop0.footprint_bytes as f64 / MIB,
    );
    metrics.insert("b2_daemon_rss_mb".into(), daemon0.rss_bytes as f64 / MIB);

    let samples = [
        stack.sample(desktop_pid, 10, "idle-desktop"),
        stack.sample(stack.daemon.pid(), 10, "idle-daemon"),
    ];
    let t0 = Instant::now();
    sleep(CPU_WINDOW).await;
    for mut sample in samples.into_iter().flatten() {
        let _ = sample.wait();
    }
    let elapsed = t0.elapsed().as_secs_f64();
    stack.check_desktop()?;
    let desktop1 = proc::usage(desktop_pid)?;
    let daemon1 = stack.daemon_usage()?;
    let pct = |a: &proc::Usage, b: &proc::Usage| (b.cpu - a.cpu).as_secs_f64() / elapsed * 100.0;
    let (desktop_pct, daemon_pct) = (pct(&desktop0, &desktop1), pct(&daemon0, &daemon1));
    metrics.insert("b3_desktop_cpu_pct".into(), desktop_pct);
    metrics.insert("b3_daemon_cpu_pct".into(), daemon_pct);
    metrics.insert("b3_idle_cpu_pct".into(), desktop_pct + daemon_pct);
    metrics.insert(
        "b3_desktop_wakeups_per_s".into(),
        (desktop1.wakeups - desktop0.wakeups) as f64 / elapsed,
    );
    metrics.insert(
        "b3_daemon_wakeups_per_s".into(),
        (daemon1.wakeups - daemon0.wakeups) as f64 / elapsed,
    );
    Ok(metrics)
}

const TYPING_KEYS: usize = 500;
const TYPING_INTERVAL_MS: u64 = 100;
const SCRIPT_START_DELAY_MS: u64 = 1500;
/// Spread of key times over one display frame at least (see `Script::jitter` in the desktop).
const KEY_JITTER_MS: u64 = 17;

async fn typing(stack: &mut Stack<'_>, key_interval_ms: Option<u64>) -> Result<Metrics> {
    let interval_ms = key_interval_ms.unwrap_or(TYPING_INTERVAL_MS);
    let session = stack.create(PROMPT_CAT).await?;
    let script = seed::typing_script(
        TYPING_KEYS,
        interval_ms,
        SCRIPT_START_DELAY_MS,
        KEY_JITTER_MS,
    );
    stack.launch_desktop(&[&session], true, Some(&script))?;
    let budget = Duration::from_millis(TYPING_KEYS as u64 * interval_ms + 30_000);
    stack
        .wait_trace("bench_start", budget, |e| is_event(e, "bench_start"))
        .await?;
    let desktop_pid = stack.desktop()?.pid();
    let samples = [
        stack.sample(desktop_pid, 8, "typing-desktop"),
        stack.sample(stack.daemon.pid(), 8, "typing-daemon"),
    ];
    stack
        .wait_trace("bench_done", budget, |e| is_event(e, "bench_done"))
        .await?;
    for mut sample in samples.into_iter().flatten() {
        let _ = sample.wait();
    }
    // The last echoes arrive after the last key was sent.
    sleep(Duration::from_secs(1)).await;
    stack.check_desktop()?;
    let totals = key_totals_ms(&stack.trace(), None);
    if totals.len() < TYPING_KEYS * 95 / 100 {
        bail!(
            "only {} of {TYPING_KEYS} keystrokes were painted",
            totals.len()
        );
    }
    let mut metrics = Metrics::new();
    record_latency(&mut metrics, "b4", &totals);
    record_stages(&mut metrics, "b4", &stack.trace(), None);
    metrics.insert("b4_window_active".into(), window_active(&stack.trace()));
    Ok(metrics)
}

/// 1.0 when the desktop window was the active (key) window when the key script began. GPUI
/// caps inactive windows at 30 fps, so latency numbers are only representative when it was.
fn window_active(events: &[Value]) -> f64 {
    let active = events
        .iter()
        .find(|e| is_event(e, "bench_start"))
        .is_some_and(|e| e["window_active"] == true);
    f64::from(u8::from(active))
}

const FLOOD_KEYS: usize = 600;
/// 20 keys/s with jitter: a fast typist. Faster than one key per ack round trip (about two
/// display frames) makes echoes queue behind acks (docs/perf.md, B5).
const FLOOD_INTERVAL_MS: u64 = 50;
const FLOOD_DEADLINE: Duration = Duration::from_secs(600);
/// Output of one `head -c 150000000 | base64`: 76 characters + newline per 57 bytes ≈ 203 MB.
/// The pipeline takes ~2 s (the daemon parses ~100 MB/s whatever the display does), too short
/// to type through, so the flood repeats `FLOOD_ROUNDS` times (≈ 1.2 GB) back to back.
const FLOOD_ROUNDS: usize = 6;
const FLOOD_COMMAND: &str = "read go; for i in $(seq {rounds}); do head -c 150000000 /dev/urandom | base64; done; touch '{done}'; exec cat";

async fn flood(stack: &mut Stack<'_>, key_interval_ms: Option<u64>) -> Result<Metrics> {
    let interval_ms = key_interval_ms.unwrap_or(FLOOD_INTERVAL_MS);
    let done = stack.path("flood-done");
    let typing = stack.create(PROMPT_CAT).await?;
    let script = FLOOD_COMMAND
        .replace("{rounds}", &FLOOD_ROUNDS.to_string())
        .replace("{done}", &done.display().to_string());
    // The flood session also prints a prompt so its pane has text before the flood starts.
    let flooded = stack
        .create(&["sh", "-c", &format!("printf 'flood> '; {script}")])
        .await?;
    let keys = seed::typing_script(
        FLOOD_KEYS,
        interval_ms,
        SCRIPT_START_DELAY_MS,
        KEY_JITTER_MS,
    );
    stack.launch_desktop(&[&typing, &flooded], true, Some(&keys))?;
    stack
        .wait_trace("bench_start", Duration::from_secs(60), |e| {
            is_event(e, "bench_start")
        })
        .await?;

    let baseline = stack.daemon_usage()?.rss_bytes;
    let flood_start = now_us();
    stack.type_into(&flooded, "go\n").await?;
    let mut peak = baseline;
    let deadline = Instant::now() + FLOOD_DEADLINE;
    while !done.exists() {
        stack.check_desktop()?;
        if Instant::now() > deadline {
            bail!("the flood did not finish in {FLOOD_DEADLINE:?}");
        }
        peak = peak.max(stack.daemon_usage()?.rss_bytes);
        sleep(Duration::from_millis(100)).await;
    }
    let flood_end = now_us();
    peak = peak.max(stack.daemon_usage()?.rss_bytes);
    stack
        .wait_trace(
            "bench_done",
            Duration::from_millis(FLOOD_KEYS as u64 * interval_ms) + Duration::from_secs(60),
            |e| is_event(e, "bench_done"),
        )
        .await?;
    sleep(Duration::from_secs(1)).await;
    stack.check_desktop()?;

    let events = stack.trace();
    let during = key_totals_ms(&events, Some((flood_start, flood_end)));
    let all = key_totals_ms(&events, None);
    if during.len() < 100 {
        bail!(
            "only {} keystrokes were painted while the flood ran ({:.1} s); the flood was too short to measure",
            during.len(),
            (flood_end - flood_start) as f64 / 1e6
        );
    }
    let mut metrics = Metrics::new();
    record_latency(&mut metrics, "b5", &during);
    record_stages(&mut metrics, "b5", &events, Some((flood_start, flood_end)));
    // Paints per second of the busiest view while flooded: the display's frame rate.
    let rates: Vec<f64> = events
        .iter()
        .filter(|e| is_event(e, "paints"))
        .filter(|e| {
            e["t_us"]
                .as_u64()
                .is_some_and(|t| flood_start <= t && t <= flood_end)
        })
        .filter_map(|e| Some(e["n"].as_f64()? / (e["span_us"].as_f64()? / 1e6)))
        .collect();
    if let Some(rate) = percentile(&rates, 100.0) {
        metrics.insert("b5_paints_per_s_max".into(), rate);
    }
    record_latency(&mut metrics, "b5_all", &all);
    metrics.insert(
        "b5_daemon_growth_mb".into(),
        peak.saturating_sub(baseline) as f64 / MIB,
    );
    metrics.insert("b5_daemon_baseline_mb".into(), baseline as f64 / MIB);
    metrics.insert("b5_daemon_peak_mb".into(), peak as f64 / MIB);
    metrics.insert(
        "b5_flood_seconds".into(),
        (flood_end - flood_start) as f64 / 1e6,
    );
    metrics.insert("b5_keys_sent".into(), FLOOD_KEYS as f64);
    Ok(metrics)
}

async fn cold_start(stack: &mut Stack<'_>) -> Result<Metrics> {
    let mut sessions = Vec::new();
    for _ in 0..SHOWN_SESSIONS {
        sessions.push(stack.create(PROMPT_CAT).await?);
    }
    // Let the prompts reach the daemon's screens before the clock starts.
    sleep(Duration::from_secs(1)).await;
    let shown: Vec<&SessionInfo> = sessions.iter().collect();
    let spawned = now_us();
    stack.launch_desktop(&shown, true, None)?;
    let painted = stack
        .wait_trace("the first terminal paint", Duration::from_secs(30), |e| {
            e["ev"] == "startup" && e["stage"] == "first_terminal_painted"
        })
        .await?;
    stack.check_desktop()?;
    let events = stack.trace();
    let since = |t: u64| t.saturating_sub(spawned) as f64 / 1000.0;
    let mut metrics = Metrics::new();
    metrics.insert(
        "b6_cold_start_ms".into(),
        since(painted["t_us"].as_u64().unwrap_or(0)),
    );
    for stage in ["main", "window_opened", "first_frame", "connected"] {
        if let Some(t) = startup_us(&events, stage) {
            metrics.insert(format!("b6_{stage}_ms"), since(t));
        }
    }
    Ok(metrics)
}
