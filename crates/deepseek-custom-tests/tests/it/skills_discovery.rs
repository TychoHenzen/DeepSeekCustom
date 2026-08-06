//! Tests for `deepseek_custom::skills::discovery`: which files count as a
//! skill, and which roots they are looked for under.
//!
//! Every test here points discovery at a fixture tree rather than the
//! running machine. The real roots hold whatever the developer happens to
//! have installed, which would make a count assertion meaningless.

use std::path::{Path, PathBuf};

use deepseek_custom::plugins::PluginRoot;
use deepseek_custom::skills::SkillSource;
use deepseek_custom::skills::discovery::discover_skill_files_in;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-skills-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a flat `<root>/skills/<name>.md`.
fn write_flat(root: &Path, name: &str, body: &str) {
    let dir = root.join("skills");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
}

/// Write a directory-form `<root>/skills/<name>/SKILL.md`.
fn write_dir_form(root: &Path, name: &str, body: &str) {
    let dir = root.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn names(found: &[(PathBuf, SkillSource)]) -> Vec<String> {
    found
        .iter()
        .map(|(p, _)| p.display().to_string().replace('\\', "/"))
        .collect()
}

#[test]
fn finds_a_flat_markdown_skill() {
    let root = temp_dir("flat");
    write_flat(&root, "explore", "body");

    let found = discover_skill_files_in(&root, None, &[]);

    assert_eq!(found.len(), 1);
    assert!(names(&found)[0].ends_with("skills/explore.md"));
    assert_eq!(found[0].1, SkillSource::Project);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn finds_a_directory_form_skill() {
    // The shape Claude Code actually writes, and the one the old loader
    // skipped outright: 95 of this machine's 99 skills use it.
    let root = temp_dir("dir-form");
    write_dir_form(&root, "tighten", "body");

    let found = discover_skill_files_in(&root, None, &[]);

    assert_eq!(found.len(), 1);
    assert!(names(&found)[0].ends_with("skills/tighten/SKILL.md"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn finds_both_shapes_in_one_root() {
    let root = temp_dir("both");
    write_flat(&root, "explore", "body");
    write_dir_form(&root, "tighten", "body");

    let found = discover_skill_files_in(&root, None, &[]);

    assert_eq!(found.len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ignores_a_directory_with_no_skill_file() {
    let root = temp_dir("empty-dir");
    std::fs::create_dir_all(root.join("skills").join("scripts")).unwrap();

    let found = discover_skill_files_in(&root, None, &[]);

    assert!(found.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ignores_a_non_markdown_file() {
    let root = temp_dir("non-md");
    let dir = root.join("skills");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a skill").unwrap();

    let found = discover_skill_files_in(&root, None, &[]);

    assert!(found.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_skills_directory_is_not_an_error() {
    let root = temp_dir("missing");

    let found = discover_skill_files_in(&root, None, &[]);

    assert!(found.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn finds_a_plugin_skill() {
    let root = temp_dir("plugin-project");
    let plugin_root = temp_dir("plugin-root");
    write_dir_form(&plugin_root, "tighten", "body");
    let plugins = vec![PluginRoot {
        name: "dod-guard".into(),
        marketplace: "dod-guard".into(),
        root: plugin_root.clone(),
    }];

    let found = discover_skill_files_in(&root, None, &plugins);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, SkillSource::Plugin("dod-guard".into()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&plugin_root);
}

#[test]
fn finds_a_global_skill() {
    let root = temp_dir("global-project");
    let global = temp_dir("global-home");
    std::fs::write(global.join("commit.md"), "body").unwrap();

    let found = discover_skill_files_in(&root, Some(&global), &[]);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, SkillSource::Global);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&global);
}

#[test]
fn roots_come_back_lowest_precedence_first() {
    // The loader keeps the last entry for a name, so the order here is what
    // makes a project skill beat a global one and a global one beat a
    // plugin's.
    let root = temp_dir("order-project");
    let global = temp_dir("order-global");
    let plugin_root = temp_dir("order-plugin");
    write_flat(&root, "shared", "project");
    std::fs::write(global.join("shared.md"), "global").unwrap();
    write_flat(&plugin_root, "shared", "plugin");
    let plugins = vec![PluginRoot {
        name: "p".into(),
        marketplace: "m".into(),
        root: plugin_root.clone(),
    }];

    let found = discover_skill_files_in(&root, Some(&global), &plugins);

    let sources: Vec<&SkillSource> = found.iter().map(|(_, s)| s).collect();
    assert_eq!(
        sources,
        vec![
            &SkillSource::Plugin("p".into()),
            &SkillSource::Global,
            &SkillSource::Project,
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&global);
    let _ = std::fs::remove_dir_all(&plugin_root);
}
