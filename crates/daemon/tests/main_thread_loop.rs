//! The main-thread run loop that hosts the native notifications (`notify_native::cocoa`): work
//! queued from another thread runs on the main thread inside the loop, and the loop ends when
//! told to, also when the order came before it started. No notification API is involved.
//!
//! The test has its own `main` (`harness = false`): only a process's main thread can serve the
//! main dispatch queue, and the default harness runs tests on other threads. It answers the
//! `--list` protocol of libtest so that nextest can run it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(target_os = "macos")]
fn main() {
    use std::sync::mpsc;
    use std::time::Duration;

    use hutan_daemon::notify_native::cocoa::{Stopper, on_main_thread, run_main_loop};

    const NAME: &str = "the_main_thread_run_loop_runs_queued_work_and_stops_when_told";

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        if !args.iter().any(|arg| arg == "--ignored") {
            println!("{NAME}: test");
        }
        return;
    }

    // A hung loop must fail the test, not the whole run.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(30));
        eprintln!("the main thread run loop did not stop in 30 s");
        std::process::exit(1);
    });

    let is_main = || objc2::MainThreadMarker::new().is_some();
    assert!(is_main(), "this test must run on the main thread");

    // 1. Work queued from another thread while the loop runs, then stop from that thread.
    let (seen, results) = mpsc::channel();
    let driver = std::thread::spawn(move || {
        assert!(
            !is_main(),
            "the driver is not the main thread, that is the point"
        );
        for n in 0..3 {
            let seen = seen.clone();
            on_main_thread(move || seen.send((n, is_main())).unwrap());
        }
        let got: Vec<(i32, bool)> = (0..3)
            .map(|_| results.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        Stopper.stop();
        got
    });
    run_main_loop();
    let got = driver.join().unwrap();
    assert_eq!(
        got,
        [(0, true), (1, true), (2, true)],
        "queued work runs in order on the main thread"
    );

    // 2. A stop that was ordered before the loop started ends it as soon as it runs.
    Stopper.stop();
    run_main_loop();

    println!("test {NAME} ... ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    if std::env::args().any(|arg| arg == "--list") {
        return;
    }
}
