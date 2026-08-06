//! Unit tests for `deepseek_custom::skills`: frontmatter parsing, the name
//! a skill takes when its frontmatter declares none, and the shape of the
//! prompt index.

use std::path::{Path, PathBuf};

use deepseek_custom::skills::{Skill, SkillSource, format_skills_for_prompt};

const SAMPLE_SKILL: &str = r#"---
name: my-skill
description: A test skill
tools:
  - bash
  - read
---

# Instructions

Do the thing carefully."#;

const NO_FRONTMATTER: &str = r#"# Plain skill

Just raw markdown content."#;

fn parse(content: &str, path: &str) -> Skill {
    Skill::from_markdown(content, Path::new(path), SkillSource::Project).unwrap()
}

fn fixture(name: &str, description: &str) -> Skill {
    Skill {
        name: name.into(),
        description: description.into(),
        tools: Vec::new(),
        path: PathBuf::from("skills/x.md"),
        source: SkillSource::Project,
    }
}

#[test]
fn parse_skill_with_frontmatter() {
    let skill = parse(SAMPLE_SKILL, "skills/test.md");
    assert_eq!(skill.name, "my-skill");
    assert_eq!(skill.description, "A test skill");
    assert_eq!(skill.tools, vec!["bash", "read"]);
}

#[test]
fn parse_skill_without_frontmatter_falls_back_to_filename() {
    let skill = parse(NO_FRONTMATTER, "skills/plain.md");
    assert_eq!(skill.name, "plain");
    assert_eq!(skill.description, "");
}

#[test]
fn skill_md_falls_back_to_its_directory_name() {
    // Every directory-form skill file is called SKILL.md, so the filename
    // cannot name it. Without this, 60-odd skills would all be "SKILL".
    let skill = parse(NO_FRONTMATTER, "skills/tighten/SKILL.md");
    assert_eq!(skill.name, "tighten");
}

#[test]
fn allowed_tools_is_read_as_tools() {
    // Claude Code's own spelling of the field. A skill written for it must
    // parse here unchanged.
    let content = "---\nname: s\nallowed-tools:\n  - Read\n  - Bash\n---\n\nbody";
    let skill = parse(content, "skills/s.md");
    assert_eq!(skill.tools, vec!["Read", "Bash"]);
}

#[test]
fn explicit_tools_wins_over_allowed_tools() {
    let content = "---\nname: s\ntools:\n  - Write\nallowed-tools:\n  - Read\n---\n\nbody";
    let skill = parse(content, "skills/s.md");
    assert_eq!(skill.tools, vec!["Write"]);
}

#[test]
fn malformed_frontmatter_still_parses() {
    // Missing closing delimiter: the whole text is body, and the name comes
    // off the path.
    let skill = parse("---\nname: broken\n", "skills/broken.md");
    assert_eq!(skill.name, "broken");
}

#[test]
fn prompt_index_lists_names_and_descriptions() {
    let skills = vec![fixture("alpha", "does alpha things")];
    let formatted = format_skills_for_prompt(&skills);
    assert!(formatted.contains("`alpha`"));
    assert!(formatted.contains("does alpha things"));
}

#[test]
fn prompt_index_names_the_skill_tool() {
    // The index is useless without telling the model how to reach a body.
    let formatted = format_skills_for_prompt(&[fixture("alpha", "d")]);
    assert!(formatted.contains("`Skill`"));
}

#[test]
fn prompt_index_omits_bodies() {
    // The whole reason the index exists. 99 skills hold 884 KB of markdown
    // against a 100000-token default budget, so a body must never land in
    // the prompt.
    let skill = Skill {
        name: "alpha".into(),
        description: "d".into(),
        tools: Vec::new(),
        path: PathBuf::from("skills/alpha.md"),
        source: SkillSource::Project,
    };
    let formatted = format_skills_for_prompt(&[skill]);
    assert!(!formatted.contains("Do the thing carefully"));
    assert!(formatted.len() < 400);
}

#[test]
fn prompt_index_trims_a_long_description() {
    let long = "word ".repeat(200);
    let formatted = format_skills_for_prompt(&[fixture("alpha", &long)]);
    assert!(formatted.contains("..."));
    assert!(formatted.len() < 400);
}

#[test]
fn prompt_index_flattens_a_multi_line_description() {
    let formatted = format_skills_for_prompt(&[fixture("alpha", "one\n  two\n  three")]);
    assert!(formatted.contains("one two three"));
}

#[test]
fn prompt_index_sorts_by_name() {
    let skills = vec![fixture("zebra", "z"), fixture("alpha", "a")];
    let formatted = format_skills_for_prompt(&skills);
    let alpha = formatted.find("`alpha`").unwrap();
    let zebra = formatted.find("`zebra`").unwrap();
    assert!(alpha < zebra);
}

#[test]
fn prompt_index_handles_a_skill_with_no_description() {
    let formatted = format_skills_for_prompt(&[fixture("alpha", "")]);
    assert!(formatted.contains("`alpha`"));
    assert!(!formatted.contains("`alpha`:"));
}

#[test]
fn format_empty_skills_returns_empty() {
    assert_eq!(format_skills_for_prompt(&[]), "");
}

#[test]
fn load_body_reads_the_file_without_frontmatter() {
    let dir = std::env::temp_dir().join(format!("dsc-skill-body-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("body.md");
    std::fs::write(&path, SAMPLE_SKILL).unwrap();

    let skill = Skill::from_markdown(SAMPLE_SKILL, &path, SkillSource::Project).unwrap();
    let body = skill.load_body().unwrap();

    assert!(body.contains("Do the thing carefully"));
    assert!(!body.contains("name: my-skill"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_body_errors_when_the_file_is_gone() {
    let skill = Skill {
        name: "gone".into(),
        description: String::new(),
        tools: Vec::new(),
        path: PathBuf::from("no/such/skill.md"),
        source: SkillSource::Project,
    };
    assert!(skill.load_body().is_err());
}

#[test]
fn source_labels_name_their_origin() {
    assert_eq!(SkillSource::Project.label(), "project");
    assert_eq!(SkillSource::Global.label(), "global");
    assert_eq!(
        SkillSource::Plugin("dod-guard".into()).label(),
        "plugin:dod-guard"
    );
}
