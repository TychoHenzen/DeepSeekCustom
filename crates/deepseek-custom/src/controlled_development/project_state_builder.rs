use super::{ControlledDevelopmentProjectStateInput, MAX_PROOF_COMMANDS, PROJECT_STATE_PATH};

pub const MAX_PROJECT_STATE_NONBLANK_LINES: usize = 40;
pub const MAX_SYSTEM_MAP_COMPONENTS: usize = 10;

pub fn build_project_state(input: &ControlledDevelopmentProjectStateInput) -> String {
    let mut lines = vec![
        "## Current outcome".to_string(),
        single_line(&input.outcome),
        String::new(),
        "## System map".to_string(),
    ];
    let system_map = input
        .system_map
        .iter()
        .filter_map(|component| {
            let name = single_line(&component.name);
            (!name.is_empty())
                .then(|| format!("- {}: {name}", single_line(&component.responsibility)))
        })
        .take(MAX_SYSTEM_MAP_COMPONENTS)
        .collect::<Vec<_>>();
    let system_map_is_empty = system_map.is_empty();
    lines.extend(system_map);
    if system_map_is_empty {
        lines.push("- none".to_string());
    }
    lines.extend([
        String::new(),
        "## Last completed Work Card".to_string(),
        format!("id: {}", single_line(&input.work_card.id)),
        format!("outcome: {}", single_line(&input.work_card.outcome)),
        format!(
            "proof_commands: {}",
            compact_json(&input.work_card.proof_commands)
        ),
        format!(
            "production_paths: {}",
            compact_json(&input.work_card.production_paths)
        ),
        format!(
            "supporting_paths: {}",
            compact_json(&input.work_card.supporting_paths)
        ),
        format!("excluded: {}", compact_json(&input.work_card.excluded)),
        format!(
            "complexity_exceptions: {}",
            compact_json(&input.work_card.complexity_exceptions)
        ),
        String::new(),
        "## Exact changed paths".to_string(),
    ]);
    let mut changed_paths = input.changed_paths.clone();
    if !changed_paths.iter().any(|path| path == PROJECT_STATE_PATH) {
        changed_paths.push(PROJECT_STATE_PATH.to_string());
    }
    changed_paths.sort();
    changed_paths.dedup();
    lines.push(compact_json(&changed_paths));
    lines.extend([
        String::new(),
        "## Last proof commands and results".to_string(),
    ]);
    lines.extend(
        input
            .proof_evidence
            .iter()
            .take(MAX_PROOF_COMMANDS)
            .map(|evidence| {
                let exit_code = evidence
                    .result
                    .as_ref()
                    .and_then(|result| result.exit_code)
                    .map_or_else(|| "none".to_string(), |code| code.to_string());
                format!(
                    "- {} => {:?} (exit {})",
                    single_line(&evidence.command),
                    evidence.disposition,
                    exit_code
                )
            }),
    );
    if input.proof_evidence.is_empty() {
        lines.push("- none".to_string());
    }
    if let Some(blocker) = input.blocker.as_deref() {
        lines.extend([
            String::new(),
            "## Known blocker".to_string(),
            single_line(blocker),
        ]);
    }
    debug_assert!(
        lines.iter().filter(|line| !line.trim().is_empty()).count()
            <= MAX_PROJECT_STATE_NONBLANK_LINES
    );
    format!("{}\n", lines.join("\n"))
}

fn compact_json(values: &[String]) -> String {
    serde_json::to_string(values).expect("bounded strings serialize to JSON")
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
