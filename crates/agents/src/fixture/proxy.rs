//! Records a real session: stands in for `codex` between Heeler and the real app server and
//! writes everything that passes to a raw transcript (`scrub` makes a fixture of it).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type Log = Arc<Mutex<File>>;

fn record(log: &Log, dir: &str, line: &str) {
    let msg: Value = serde_json::from_str(line).unwrap_or_else(|_| json!({"raw": line}));
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let mut file = log.lock().unwrap_or_else(PoisonError::into_inner);
    // A recording that cannot be written must not break the session being recorded.
    let _ = writeln!(file, "{}", json!({"dir": dir, "t": t, "msg": msg}));
}

/// Runs `program args...`, forwarding stdio and recording both directions into `out`.
pub fn run(out: &std::path::Path, program: &str, args: &[String]) -> io::Result<i32> {
    let log: Log = Arc::new(Mutex::new(File::create(out)?));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let (Some(mut child_in), Some(child_out)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("child pipes missing"));
    };
    let up = {
        let log = log.clone();
        thread::spawn(move || {
            for line in io::stdin().lock().lines().map_while(Result::ok) {
                record(&log, "out", &line);
                if writeln!(child_in, "{line}")
                    .and_then(|()| child_in.flush())
                    .is_err()
                {
                    break;
                }
            }
            // Dropping `child_in` closes the child's stdin: our client went away.
        })
    };
    let down = {
        let log = log.clone();
        thread::spawn(move || {
            let mut stdout = io::stdout();
            for line in BufReader::new(child_out).lines().map_while(Result::ok) {
                record(&log, "in", &line);
                if writeln!(stdout, "{line}")
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
    };
    let status = child.wait()?;
    // The reader of the child's output ends with the child; the input thread may sit in a
    // blocking read of our stdin, so it is left to die with the process.
    let _ = down.join();
    drop(up);
    Ok(status.code().unwrap_or(1))
}
