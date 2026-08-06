//! Unit tests for `deepseek_custom::skills`, moved out of the production
//! module as part of the two-crate workspace split.

use deepseek_custom::skills::{Skill, format_skills_for_prompt};

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

#[test]
fn parse_skill_with_frontmatter() {
    let skill = Skill::from_markdown(SAMPLE_SKILL, "test.md").unwrap();
    assert_eq!(skill.name, "my-skill");
    assert_eq!(skill.description, "A test skill");
    assert_eq!(skill.tools, vec!["bash", "read"]);
    assert!(skill.content.contains("Do the thing carefully"));
}

#[test]
fn parse_skill_without_frontmatter() {
    let skill = Skill::from_markdown(NO_FRONTMATTER, "plain.md").unwrap();
    assert_eq!(skill.name, "plain"); // filename without .md
    assert!(skill.content.contains("Just raw markdown"));
}

#[test]
fn format_skills_produces_output() {
    let skills = vec![Skill {
        name: "test".into(),
        description: "desc".into(),
        tools: vec!["bash".into()],
        content: "Do stuff".into(),
    }];
    let formatted = format_skills_for_prompt(&skills);
    assert!(formatted.contains("### test"));
    assert!(formatted.contains("Do stuff"));
}

#[test]
fn format_empty_skills_returns_empty() {
    assert_eq!(format_skills_for_prompt(&[]), "");
}

#[test]
fn malformed_skill_handled_gracefully() {
    // Missing closing --- delimiter
    let content = "---\nname: broken\n";
    let skill = Skill::from_markdown(content, "broken.md").unwrap();
    // Falls through, frontmatter delimiter not found
    assert!(skill.name == "broken" || !skill.content.is_empty());
}
