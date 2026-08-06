//! Tests for `deepseek_custom::process_group` (`src/process_group.rs`).
//!
//! The guarantee under test cannot be checked inside the test process: it
//! is "when this process dies, its children die too", and a test that dies
//! reports nothing. So the check runs one process out. `orphan_probe`
//! (`src/bin/orphan_probe.rs`) plays the harness: it spawns a child, adopts
//! it, prints the child's id, and exits through `std::process::exit`, which
//! runs no destructor. Anything still running after that was reaped by the
//! operating system or not at all.
//!
//! The probe's output is read one line at a time rather than through
//! `Command::output`. That is not a style choice. `output` waits for end of
//! file on the pipe, and an orphaned grandchild can hold that pipe open, so
//! the first version of this test sat for the full 300 seconds the orphan
//! ran and then passed because the orphan had by then exited on its own.
//! It passed with the fix reverted, which is the definition of a test that
//! proves nothing.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for the operating system to reap the orphan. The
/// probe's child is told to run far longer than this, so a live process at
/// the end of this window is a real failure and not a race.
const REAP_TIMEOUT: Duration = Duration::from_secs(10);

/// Ask Windows whether a process id is still live. `tasklist` prints a
/// header and a "no tasks" line rather than failing when nothing matches,
/// so the id itself has to appear in the output for this to be true.
#[cfg(windows)]
fn is_alive(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist should run");
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

/// Run the probe and return the process id it left behind, once the probe
/// itself has exited.
#[cfg(windows)]
fn run_probe() -> u32 {
    let mut probe = Command::new(env!("CARGO_BIN_EXE_orphan_probe"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the probe should start");

    let mut line = String::new();
    BufReader::new(probe.stdout.take().expect("piped stdout"))
        .read_line(&mut line)
        .expect("the probe should print a line");

    let status = probe.wait().expect("the probe should exit");
    assert!(status.success(), "probe exited with {status}");

    line.trim()
        .parse()
        .unwrap_or_else(|_| panic!("expected a process id, got {line:?}"))
}

#[cfg(windows)]
#[test]
fn a_child_dies_with_the_process_that_adopted_it() {
    let pid = run_probe();

    // Termination is the kernel's work once the last job handle closes, so
    // it is not instant. Poll rather than sleep a fixed time, which would
    // trade a slow machine for a false failure.
    let deadline = Instant::now() + REAP_TIMEOUT;
    while is_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }

    if is_alive(pid) {
        // Leave nothing behind for the next run, then fail.
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
        panic!("child {pid} outlived the process that adopted it");
    }
}
