//! Tests for `deepseek_custom::mcp::spawn`: turning a configured command
//! name into something Windows will actually start.
//!
//! These drive `resolve_windows` against an explicit search path, so they
//! run and mean the same thing on any platform. The real `resolve_command`
//! reads `PATH` and `PATHEXT`, which would make an assertion depend on what
//! the machine running the test happens to have installed.

use std::path::{Path, PathBuf};

use deepseek_custom::mcp::spawn::resolve_windows;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-spawn-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "").unwrap();
}

fn extensions() -> Vec<String> {
    vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]
}

#[test]
fn a_batch_command_is_run_through_cmd() {
    // The real failure this exists for: `npx` is `npx.cmd`, and the
    // `dod-guard` server was the one of seven that would not start.
    let dir = temp_dir("batch");
    touch(&dir, "npx.CMD");

    let resolved = resolve_windows("npx", &[dir.clone()], &extensions());

    assert_eq!(resolved.program, "cmd");
    assert_eq!(resolved.prefix_args[0], "/c");
    assert!(resolved.prefix_args[1].to_lowercase().ends_with("npx.cmd"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bat_file_is_run_through_cmd_too() {
    let dir = temp_dir("bat");
    touch(&dir, "thing.BAT");

    let resolved = resolve_windows("thing", &[dir.clone()], &extensions());

    assert_eq!(resolved.program, "cmd");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_executable_is_run_directly_by_its_full_path() {
    let dir = temp_dir("exe");
    touch(&dir, "uvx.EXE");

    let resolved = resolve_windows("uvx", &[dir.clone()], &extensions());

    assert!(resolved.prefix_args.is_empty());
    assert!(resolved.program.to_lowercase().ends_with("uvx.exe"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn extensions_are_tried_in_order() {
    // PATHEXT order decides which of two same-named files wins, the same
    // way it does for the shell.
    let dir = temp_dir("order");
    touch(&dir, "tool.EXE");
    touch(&dir, "tool.CMD");

    let resolved = resolve_windows("tool", &[dir.clone()], &extensions());

    assert!(resolved.prefix_args.is_empty(), "the .EXE should win");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directories_are_searched_in_order() {
    let first = temp_dir("dir-first");
    let second = temp_dir("dir-second");
    touch(&second, "tool.EXE");
    touch(&first, "tool.EXE");

    let resolved = resolve_windows("tool", &[first.clone(), second.clone()], &extensions());

    assert!(resolved.program.starts_with(&first.display().to_string()));
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn a_command_with_an_explicit_extension_is_taken_as_given() {
    let dir = temp_dir("explicit");
    touch(&dir, "node.exe");

    let resolved = resolve_windows("node.exe", &[dir.clone()], &extensions());

    assert_eq!(resolved.program, "node.exe");
    assert!(resolved.prefix_args.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_explicit_batch_path_still_goes_through_cmd() {
    // A config may name a `.cmd` outright. It is still a batch file.
    let dir = temp_dir("explicit-batch");
    touch(&dir, "run.cmd");
    let path = dir.join("run.cmd").display().to_string();

    let resolved = resolve_windows(&path, &[], &extensions());

    assert_eq!(resolved.program, "cmd");
    assert_eq!(resolved.prefix_args[1], path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_absolute_path_is_not_searched_for() {
    let resolved = resolve_windows("C:/tools/node", &[], &extensions());
    assert_eq!(resolved.program, "C:/tools/node");
    assert!(resolved.prefix_args.is_empty());
}

#[test]
fn an_unresolvable_command_is_handed_back_unchanged() {
    // Better to let the operating system report "not found" than to invent
    // a message here.
    let resolved = resolve_windows("definitely-not-real", &[], &extensions());
    assert_eq!(resolved.program, "definitely-not-real");
    assert!(resolved.prefix_args.is_empty());
}

#[test]
fn a_directory_is_not_mistaken_for_a_command() {
    let dir = temp_dir("dir-match");
    std::fs::create_dir_all(dir.join("tool.EXE")).unwrap();

    let resolved = resolve_windows("tool", &[dir.clone()], &extensions());

    assert_eq!(resolved.program, "tool");
    let _ = std::fs::remove_dir_all(&dir);
}
