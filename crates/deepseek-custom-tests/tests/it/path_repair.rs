//! Unit tests for `deepseek_custom::path_repair` (`src/path_repair.rs`).
//!
//! `repair_path` itself reads the real registry and writes the process
//! environment, so it is checked by the `path_repair_probe` example rather
//! than here. Every rule it applies to the list is a pure function, and
//! those are what this file pins: the length ceiling `cmd.exe` imposes is
//! the whole reason the module exists.

use deepseek_custom::path_repair::{
    CMD_PATH_LIMIT, append_missing, dedupe, joined_len, last_index_to_drop, same_dir, split_path,
    trim_to_limit,
};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// One directory long enough that a handful of them cross the ceiling.
fn padding(index: usize) -> String {
    format!("C:\\pad\\directory_number_{index:0>60}")
}

fn over_limit_list(count: usize) -> Vec<String> {
    (0..count).map(padding).collect()
}

#[test]
fn split_path_drops_empty_and_trims() {
    let entries = split_path("C:\\one; C:\\two ;;C:\\three;");
    assert_eq!(entries, strings(&["C:\\one", "C:\\two", "C:\\three"]));
}

#[test]
fn same_dir_ignores_a_trailing_separator() {
    assert!(same_dir(
        "C:\\Program Files\\nodejs\\",
        "C:\\Program Files\\nodejs"
    ));
}

#[cfg(windows)]
#[test]
fn same_dir_ignores_case_on_windows() {
    assert!(same_dir("C:\\Windows\\System32", "c:\\windows\\system32"));
}

#[test]
fn dedupe_keeps_the_first_appearance_and_its_order() {
    let entries = strings(&["C:\\a", "C:\\b", "C:\\a\\", "C:\\c", "C:\\b"]);
    assert_eq!(dedupe(&entries), strings(&["C:\\a", "C:\\b", "C:\\c"]));
}

#[test]
fn dedupe_leaves_a_list_with_no_repeats_alone() {
    let entries = strings(&["C:\\a", "C:\\b", "C:\\c"]);
    assert_eq!(dedupe(&entries), entries);
}

#[test]
fn append_missing_adds_only_what_the_list_lacks() {
    let mut entries = strings(&["C:\\a", "C:\\b"]);
    append_missing(&mut entries, &strings(&["C:\\b\\", "C:\\registry"]));
    assert_eq!(entries, strings(&["C:\\a", "C:\\b", "C:\\registry"]));
}

#[test]
fn joined_len_matches_the_string_the_repair_writes() {
    let entries = strings(&["C:\\a", "C:\\bb", "C:\\ccc"]);
    let separator = if cfg!(windows) { ";" } else { ":" };
    assert_eq!(joined_len(&entries), entries.join(separator).len());
}

#[test]
fn joined_len_of_an_empty_list_is_zero() {
    assert_eq!(joined_len(&[]), 0);
}

#[test]
fn last_index_to_drop_picks_the_last_entry_the_registry_does_not_name() {
    let entries = strings(&[
        "C:\\extra",
        "C:\\registry",
        "C:\\other_extra",
        "C:\\registry_two",
    ]);
    let registry = strings(&["C:\\registry", "C:\\registry_two"]);
    assert_eq!(last_index_to_drop(&entries, &registry), Some(2));
}

#[test]
fn last_index_to_drop_falls_back_to_the_final_entry() {
    let entries = strings(&["C:\\registry", "C:\\registry_two"]);
    assert_eq!(last_index_to_drop(&entries, &entries), Some(1));
}

#[test]
fn last_index_to_drop_reports_nothing_for_an_empty_list() {
    assert_eq!(last_index_to_drop(&[], &[]), None);
}

#[test]
fn trim_to_limit_leaves_a_short_list_untouched() {
    let mut entries = strings(&["C:\\a", "C:\\b"]);
    let before = entries.clone();
    trim_to_limit(&mut entries, &[]);
    assert_eq!(entries, before);
}

#[test]
fn trim_to_limit_cuts_a_long_list_under_the_shell_ceiling() {
    let mut entries = over_limit_list(200);
    assert!(joined_len(&entries) > CMD_PATH_LIMIT);
    trim_to_limit(&mut entries, &[]);
    assert!(joined_len(&entries) <= CMD_PATH_LIMIT);
    // It cut from the end, so the head of the list survived intact.
    assert_eq!(entries[0], padding(0));
}

#[test]
fn trim_to_limit_gives_up_an_extra_before_a_registry_directory() {
    let registry = strings(&["C:\\Windows\\System32"]);
    let mut entries = over_limit_list(200);
    entries.insert(0, registry[0].clone());

    trim_to_limit(&mut entries, &registry);

    assert!(joined_len(&entries) <= CMD_PATH_LIMIT);
    assert!(entries.contains(&registry[0]));
}

#[test]
fn trim_to_limit_still_cuts_when_the_registry_names_every_entry() {
    let entries = over_limit_list(200);
    let mut trimmed = entries.clone();
    trim_to_limit(&mut trimmed, &entries);
    assert!(joined_len(&trimmed) <= CMD_PATH_LIMIT);
}

#[test]
fn a_deduped_bloated_list_fits_without_any_trimming() {
    // The real failure: one launcher stacked the same directories on every
    // nested shell until the list passed the ceiling. Dropping the repeats
    // is enough on its own, so nothing has to be given up.
    let distinct = over_limit_list(40);
    let mut bloated = Vec::new();
    for _ in 0..10 {
        bloated.extend(distinct.clone());
    }
    assert!(joined_len(&bloated) > CMD_PATH_LIMIT);

    let mut entries = dedupe(&bloated);
    let after_dedupe = entries.clone();
    trim_to_limit(&mut entries, &[]);

    assert_eq!(entries, after_dedupe);
    assert_eq!(entries, distinct);
}
