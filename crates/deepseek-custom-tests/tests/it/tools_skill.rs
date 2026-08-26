//! Tests for `deepseek_custom::tools::skill`: the tool that reads one
//! skill's body on demand, which is the other half of the prompt index.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use deepseek_custom::skills::{Skill, SkillSource};
use deepseek_custom::tools::Tool;
use deepseek_custom::tools::skill::SkillTool;
use serde_json::json;

fn temp_dir(tag: &str) -> PathBuf {
    super::unique_temp_dir("dsc-skilltool", tag)
}

fn skill_in(dir: &Path, name: &str, body: &str) -> Skill {
    let path = dir.join(format!("{name}.md"));
    std::fs::write(&path, format!("---\nname: {name}\n---\n\n{body}")).unwrap();
    Skill {
        name: name.to_string(),
        description: String::new(),
        tools: Vec::new(),
        path,
        source: SkillSource::Project,
    }
}

#[tokio::test]
async fn returns_the_named_skill_body() {
    let dir = temp_dir("body");
    let tool = SkillTool::new(Arc::new(vec![skill_in(&dir, "tighten", "Run the loop.")]));

    let out = tool.execute(json!({ "name": "tighten" })).await.unwrap();

    assert!(!out.is_error);
    assert!(out.content.contains("Run the loop."));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_result_names_the_skill_it_loaded() {
    let dir = temp_dir("header");
    let tool = SkillTool::new(Arc::new(vec![skill_in(&dir, "tighten", "body")]));

    let out = tool.execute(json!({ "name": "tighten" })).await.unwrap();

    assert!(out.content.contains("tighten"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_body_comes_back_without_its_frontmatter() {
    let dir = temp_dir("no-frontmatter");
    let tool = SkillTool::new(Arc::new(vec![skill_in(&dir, "tighten", "body text")]));

    let out = tool.execute(json!({ "name": "tighten" })).await.unwrap();

    assert!(!out.content.contains("---"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_name_matches_case_insensitively() {
    // The model quotes a name back off the index, and a capitalised guess
    // must not be a dead end.
    let dir = temp_dir("case");
    let tool = SkillTool::new(Arc::new(vec![skill_in(&dir, "tighten", "body")]));

    let out = tool.execute(json!({ "name": "Tighten" })).await.unwrap();

    assert!(!out.is_error);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unknown_name_lists_the_ones_that_exist() {
    // A model that guessed cannot correct itself without seeing the real
    // names, the same reason the Task tool lists known backends.
    let dir = temp_dir("unknown");
    let tool = SkillTool::new(Arc::new(vec![
        skill_in(&dir, "tighten", "body"),
        skill_in(&dir, "ratchet", "body"),
    ]));

    let out = tool.execute(json!({ "name": "nope" })).await.unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("tighten"));
    assert!(out.content.contains("ratchet"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unknown_name_with_no_skills_loaded_says_so() {
    let tool = SkillTool::new(Arc::new(Vec::new()));

    let out = tool.execute(json!({ "name": "nope" })).await.unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("none loaded"));
}

#[tokio::test]
async fn a_missing_name_parameter_is_a_tool_error() {
    let tool = SkillTool::new(Arc::new(Vec::new()));

    let out = tool.execute(json!({})).await.unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("name"));
}

#[tokio::test]
async fn a_skill_whose_file_vanished_is_a_tool_error_not_a_failure() {
    // A turn must survive a skill file that moved after discovery.
    let skill = Skill {
        name: "gone".into(),
        description: String::new(),
        tools: Vec::new(),
        path: PathBuf::from("no/such/skill.md"),
        source: SkillSource::Project,
    };
    let tool = SkillTool::new(Arc::new(vec![skill]));

    let out = tool.execute(json!({ "name": "gone" })).await.unwrap();

    assert!(out.is_error);
    assert!(out.content.contains("gone"));
}

#[test]
fn the_schema_requires_a_name() {
    let tool = SkillTool::new(Arc::new(Vec::new()));
    let schema = tool.input_schema();
    assert_eq!(schema["required"][0], "name");
    assert_eq!(schema["properties"]["name"]["type"], "string");
}

#[test]
fn the_tool_is_registered_as_skill() {
    let tool = SkillTool::new(Arc::new(Vec::new()));
    assert_eq!(tool.name(), "Skill");
}
