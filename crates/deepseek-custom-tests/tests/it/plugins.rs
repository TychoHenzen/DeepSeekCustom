//! Tests for `deepseek_custom::plugins`: resolving enabled Claude Code
//! plugins to the directories they were installed into.

use std::path::{Path, PathBuf};

use deepseek_custom::plugins::enabled_plugin_roots_in;

fn temp_dir(tag: &str) -> PathBuf {
    super::unique_temp_dir("dsc-plugins", tag)
}

/// Build a `~/.claude` lookalike: `settings.json` with an `enabledPlugins`
/// block, and `plugins/installed_plugins.json` with an install path each.
fn write_claude_dir(claude: &Path, enabled: &str, installed: &str) {
    std::fs::create_dir_all(claude.join("plugins")).unwrap();
    std::fs::write(
        claude.join("settings.json"),
        format!("{{\"enabledPlugins\": {enabled}}}"),
    )
    .unwrap();
    std::fs::write(
        claude.join("plugins").join("installed_plugins.json"),
        format!("{{\"version\": 2, \"plugins\": {installed}}}"),
    )
    .unwrap();
}

fn install_path_json(path: &Path) -> String {
    // A Windows path carries backslashes, which JSON needs escaped.
    format!(
        "[{{\"scope\": \"user\", \"installPath\": \"{}\"}}]",
        path.display().to_string().replace('\\', "\\\\")
    )
}

#[test]
fn resolves_an_enabled_plugin_to_its_install_path() {
    let claude = temp_dir("resolve");
    let install = temp_dir("resolve-install");
    write_claude_dir(
        &claude,
        "{\"dod-guard@dod-guard\": true}",
        &format!(
            "{{\"dod-guard@dod-guard\": {}}}",
            install_path_json(&install)
        ),
    );

    let roots = enabled_plugin_roots_in(&claude);

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "dod-guard");
    assert_eq!(roots[0].marketplace, "dod-guard");
    assert_eq!(roots[0].root, install);
    let _ = std::fs::remove_dir_all(&claude);
    let _ = std::fs::remove_dir_all(&install);
}

#[test]
fn skips_a_disabled_plugin() {
    let claude = temp_dir("disabled");
    let install = temp_dir("disabled-install");
    write_claude_dir(
        &claude,
        "{\"dod-guard@dod-guard\": false}",
        &format!(
            "{{\"dod-guard@dod-guard\": {}}}",
            install_path_json(&install)
        ),
    );

    assert!(enabled_plugin_roots_in(&claude).is_empty());
    let _ = std::fs::remove_dir_all(&claude);
    let _ = std::fs::remove_dir_all(&install);
}

#[test]
fn skips_an_enabled_plugin_that_was_never_installed() {
    let claude = temp_dir("uninstalled");
    write_claude_dir(&claude, "{\"ghost@market\": true}", "{}");

    assert!(enabled_plugin_roots_in(&claude).is_empty());
    let _ = std::fs::remove_dir_all(&claude);
}

#[test]
fn skips_an_install_path_that_no_longer_exists() {
    // An uninstall removes the directory but can leave the record behind.
    let claude = temp_dir("stale");
    write_claude_dir(
        &claude,
        "{\"ghost@market\": true}",
        "{\"ghost@market\": [{\"installPath\": \"C:\\\\no\\\\such\\\\dir\"}]}",
    );

    assert!(enabled_plugin_roots_in(&claude).is_empty());
    let _ = std::fs::remove_dir_all(&claude);
}

#[test]
fn takes_the_first_install_path_that_exists() {
    let claude = temp_dir("first-real");
    let install = temp_dir("first-real-install");
    write_claude_dir(
        &claude,
        "{\"p@m\": true}",
        &format!(
            "{{\"p@m\": [{{\"installPath\": \"C:\\\\gone\"}}, {{\"scope\": \"user\", \"installPath\": \"{}\"}}]}}",
            install.display().to_string().replace('\\', "\\\\")
        ),
    );

    let roots = enabled_plugin_roots_in(&claude);

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].root, install);
    let _ = std::fs::remove_dir_all(&claude);
    let _ = std::fs::remove_dir_all(&install);
}

#[test]
fn a_key_without_a_marketplace_still_resolves() {
    let claude = temp_dir("no-market");
    let install = temp_dir("no-market-install");
    write_claude_dir(
        &claude,
        "{\"solo\": true}",
        &format!("{{\"solo\": {}}}", install_path_json(&install)),
    );

    let roots = enabled_plugin_roots_in(&claude);

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "solo");
    assert_eq!(roots[0].marketplace, "");
    let _ = std::fs::remove_dir_all(&claude);
    let _ = std::fs::remove_dir_all(&install);
}

#[test]
fn a_missing_claude_directory_yields_no_plugins() {
    // The normal case on a machine without Claude Code installed.
    assert!(enabled_plugin_roots_in(Path::new("C:/no/such/claude/dir")).is_empty());
}

#[test]
fn unparseable_json_yields_no_plugins_rather_than_failing() {
    let claude = temp_dir("bad-json");
    std::fs::create_dir_all(claude.join("plugins")).unwrap();
    std::fs::write(claude.join("settings.json"), "{ not json").unwrap();

    assert!(enabled_plugin_roots_in(&claude).is_empty());
    let _ = std::fs::remove_dir_all(&claude);
}

#[test]
fn roots_come_back_sorted_by_name() {
    let claude = temp_dir("sorted");
    let a = temp_dir("sorted-a");
    let z = temp_dir("sorted-z");
    write_claude_dir(
        &claude,
        "{\"zebra@m\": true, \"alpha@m\": true}",
        &format!(
            "{{\"zebra@m\": {}, \"alpha@m\": {}}}",
            install_path_json(&z),
            install_path_json(&a)
        ),
    );

    let roots = enabled_plugin_roots_in(&claude);

    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].name, "alpha");
    assert_eq!(roots[1].name, "zebra");
    let _ = std::fs::remove_dir_all(&claude);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&z);
}
