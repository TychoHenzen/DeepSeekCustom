//! Stands in for the harness in `tests/process_group.rs`.
//!
//! Spawns a child that would happily outlive it, hands that child to
//! `process_group::adopt`, prints the child's process id, and then exits
//! through `std::process::exit`. That skips every destructor, so
//! `kill_on_drop` cannot fire and the job object is the only thing left
//! that could reap the child. It is the closest thing to a crash a test can
//! stage on purpose.
//!
//! The test reads the printed id back and checks the child is gone.

use std::process::Stdio;

use tokio::process::Command;

#[tokio::main]
async fn main() {
    // A process that sits there for a long time, so "still alive" is
    // unambiguous when the test looks a moment later. `ping` is on every
    // Windows install and needs no shell quoting games.
    let child = Command::new("ping")
        .args(["-n", "300", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("probe could not spawn its child");

    deepseek_custom::process_group::adopt(&child);

    println!("{}", child.id().expect("child should still be running"));

    // Not `return`: falling out of main would drop `child` and let
    // `kill_on_drop` do the work, which would prove nothing about the job
    // object. Exiting here leaves the child alive as far as this process
    // is concerned.
    std::process::exit(0);
}
