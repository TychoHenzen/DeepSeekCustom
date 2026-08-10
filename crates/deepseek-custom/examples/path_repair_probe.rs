//! Print what `path_repair::repair_path` does to the current PATH.
//! Run with a deliberately bloated PATH to check the repair for real. The
//! character counts are the reading that matters: `cmd.exe` searches at
//! most 8191 characters of the list.
fn main() {
    // SAFETY: single-threaded, first statement of a probe binary.
    let report = unsafe { deepseek_custom::path_repair::repair_path() };
    println!(
        "before={} entries / {} chars, after={} entries / {} chars, node={}",
        report.before,
        report.before_len,
        report.after,
        report.after_len,
        report.node.as_deref().unwrap_or("not found")
    );
}
