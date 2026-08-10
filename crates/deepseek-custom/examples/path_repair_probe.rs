//! Print what `path_repair::repair_path` does to the current PATH.
//! Run with a deliberately broken PATH to check the repair for real.
fn main() {
    // SAFETY: single-threaded, first statement of a probe binary.
    let report = unsafe { deepseek_custom::path_repair::repair_path() };
    println!(
        "before={} after={} node={}",
        report.before,
        report.after,
        report.node.as_deref().unwrap_or("not found")
    );
}
