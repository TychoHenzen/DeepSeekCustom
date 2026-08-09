//! Tests for `deepseek_custom::skills::loader`: turning discovered files
//! into `Skill` values, and what happens when two roots claim one name.

use std::path::{Path, PathBuf};

use deepseek_custom::skills::{SkillLoader, SkillSource};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dsc-loader-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn parses_every_discovered_file() {
    let dir = temp_dir("parse-all");
    let a = write(
        &dir,
        "a.md",
        "---\nname: alpha\ndescription: A\n---\n\nbody a",
    );
    let b = write(
        &dir,
        "b.md",
        "---\nname: beta\ndescription: B\n---\n\nbody b",
    );

    let skills = SkillLoader::parse_all(vec![(a, SkillSource::Project), (b, SkillSource::Project)]);

    let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["alpha", "beta"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_later_root_overrides_an_earlier_one_by_name() {
    // Discovery hands over plugins, then global, then project. A project
    // skill named the same as a plugin's must win.
    let dir = temp_dir("override");
    let plugin = write(
        &dir,
        "p.md",
        "---\nname: shared\ndescription: from plugin\n---\n",
    );
    let project = write(
        &dir,
        "j.md",
        "---\nname: shared\ndescription: from project\n---\n",
    );

    let skills = SkillLoader::parse_all(vec![
        (plugin, SkillSource::Plugin("p".into())),
        (project, SkillSource::Project),
    ]);

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].description, "from project");
    assert_eq!(skills[0].source, SkillSource::Project);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_overridden_skill_keeps_its_original_position() {
    let dir = temp_dir("position");
    let first = write(&dir, "1.md", "---\nname: alpha\n---\n");
    let second = write(&dir, "2.md", "---\nname: beta\n---\n");
    let alpha_again = write(&dir, "3.md", "---\nname: alpha\ndescription: newer\n---\n");

    let skills = SkillLoader::parse_all(vec![
        (first, SkillSource::Plugin("p".into())),
        (second, SkillSource::Plugin("p".into())),
        (alpha_again, SkillSource::Project),
    ]);

    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0].name, "alpha");
    assert_eq!(skills[0].description, "newer");
    assert_eq!(skills[1].name, "beta");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_file_is_skipped_not_fatal() {
    // One bad skill must never hide the other 98.
    let dir = temp_dir("unreadable");
    let good = write(&dir, "good.md", "---\nname: good\n---\n");
    let missing = dir.join("does-not-exist.md");

    let skills = SkillLoader::parse_all(vec![
        (missing, SkillSource::Project),
        (good, SkillSource::Project),
    ]);

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "good");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_file_list_yields_no_skills() {
    assert!(SkillLoader::parse_all(Vec::new()).is_empty());
}

#[test]
fn a_loaded_skill_keeps_the_path_its_body_lives_at() {
    let dir = temp_dir("path");
    let path = write(&dir, "a.md", "---\nname: alpha\n---\n\nthe body");

    let skills = SkillLoader::parse_all(vec![(path.clone(), SkillSource::Project)]);

    assert_eq!(skills[0].path, path);
    assert_eq!(skills[0].load_body().unwrap(), "the body");
    let _ = std::fs::remove_dir_all(&dir);
}
