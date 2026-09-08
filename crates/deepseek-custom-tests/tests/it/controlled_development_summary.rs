use deepseek_custom::application::dto::{
    ControlledDevelopmentRawDetailKind, ControlledDevelopmentView,
};
use deepseek_custom::controlled_development::{
    ControlledDevelopmentCompactEvidence, ControlledDevelopmentCoordinator,
    ControlledDevelopmentPhase, ControlledDevelopmentSessionRecord, MAX_COMPLETION_SUMMARY_WORDS,
    MAX_PROGRESS_NOTICE_WORDS, WorkCard, build_completion_summary, build_progress_notice,
    whitespace_word_count,
};
use deepseek_custom::procedure::{
    BoundedVerifierOutput, VerifierCommandDisposition, VerifierCommandEvidence,
    VerifierGateDisposition, VerifierGateEvidence,
};

// covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Visible progress and completion summaries respect their limits
#[test]
fn typed_progress_and_completion_summaries_respect_word_limits_without_partial_values() {
    let oversized_outcome = "x ".repeat(249).trim_end().to_string();
    let oversized_path = "p ".repeat(250).trim_end().to_string();
    let card = WorkCard {
        id: "card-maximum".into(),
        outcome: oversized_outcome.clone(),
        proof_commands: vec!["backend text is not a summary input".into()],
        production_paths: vec!["src/lib.rs".into()],
        supporting_paths: vec![],
        excluded: vec!["settings.json".into()],
        complexity_exceptions: vec![],
    };
    let proofs = vec![
        proof(VerifierGateDisposition::Passed),
        proof(VerifierGateDisposition::Failed),
        proof(VerifierGateDisposition::Interrupted),
        proof(VerifierGateDisposition::NotRun { blocked_by: 1 }),
    ];

    let progress = build_progress_notice(
        ControlledDevelopmentPhase::Executing,
        Some(&card),
        std::slice::from_ref(&oversized_path),
        &proofs,
        true,
    );
    let completion = build_completion_summary(
        ControlledDevelopmentPhase::Blocked,
        Some(&card),
        std::slice::from_ref(&oversized_path),
        &proofs,
        true,
        "Proof commands can address absolute paths outside the disposable workspace.",
    )
    .unwrap();

    assert!(whitespace_word_count(&progress) <= MAX_PROGRESS_NOTICE_WORDS);
    assert!(whitespace_word_count(&completion) <= MAX_COMPLETION_SUMMARY_WORDS);
    assert!(progress.contains("failure is recorded in the blocker field"));
    assert!(completion.contains("1 passed, 1 failed, 1 interrupted, 1 not run"));
    assert!(completion.contains("Complete failure evidence is shown"));
    assert!(!completion.contains(&oversized_outcome));
    assert!(!completion.contains(&oversized_path));
    assert!(!completion.contains("x x"));
    assert!(!completion.contains("p p"));
}

fn proof(disposition: VerifierGateDisposition) -> VerifierGateEvidence {
    VerifierGateEvidence {
        command: "command kept outside the compact summary".into(),
        disposition,
        result: None,
    }
}

// covers: deepseek-custom/controlled-development-mode :: Compact output is bounded without hiding diagnostics :: Raw diagnostic output remains available
#[test]
fn raw_projection_preserves_backend_verifier_and_failure_evidence_byte_for_byte() {
    let reasoning = "reasoning ".repeat(300);
    let tool = "tool-output\n".repeat(300);
    let assistant = "assistant text ".repeat(300);
    let stdout = "verifier stdout\n".repeat(300);
    let stderr = "verifier stderr\n".repeat(300);
    let failure = "failure evidence ".repeat(300);
    let gate = VerifierGateEvidence {
        command: "cargo test focused".into(),
        disposition: VerifierGateDisposition::Failed,
        result: Some(VerifierCommandEvidence {
            command: "cargo test focused".into(),
            disposition: VerifierCommandDisposition::Failed,
            success: false,
            exit_code: Some(1),
            stdout: output(&stdout),
            stderr: output(&stderr),
            combined_output: output(&format!("{stdout}{stderr}")),
            duration_millis: 7,
            error: Some("verifier process returned an error".into()),
        }),
    };
    let mut coordinator = ControlledDevelopmentCoordinator::default();
    coordinator
        .install_session_record(ControlledDevelopmentSessionRecord {
            state: Default::default(),
            compact_evidence: ControlledDevelopmentCompactEvidence {
                blocker: Some(failure.clone()),
                proof_evidence: vec![gate],
                ..Default::default()
            },
            raw_details: vec![reasoning.clone(), tool.clone(), assistant.clone()],
            retained_workspace: None,
        })
        .unwrap();

    let view = ControlledDevelopmentView::from_coordinator(&coordinator);
    let contents = view
        .raw_details
        .iter()
        .map(|detail| detail.content.as_str())
        .collect::<Vec<_>>();
    assert!(contents.contains(&reasoning.as_str()));
    assert!(contents.contains(&tool.as_str()));
    assert!(contents.contains(&assistant.as_str()));
    assert!(contents.contains(&stdout.as_str()));
    assert!(contents.contains(&stderr.as_str()));
    assert!(contents.contains(&failure.as_str()));
    assert!(view.raw_details.iter().any(|detail| detail.kind == ControlledDevelopmentRawDetailKind::VerifierCombinedOutput));
    assert!(
        view.raw_details
            .iter()
            .all(|detail| !detail.content.ends_with('…'))
    );
}

fn output(text: &str) -> BoundedVerifierOutput {
    BoundedVerifierOutput {
        text: text.into(),
        first_edge: text.into(),
        last_edge: String::new(),
        truncated: false,
        bytes_seen: text.len() as u64,
    }
}
