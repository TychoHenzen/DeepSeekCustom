use deepseek_custom::procedure::{
    LocalizationAttempt, LocalizationTarget, ProcedureAttemptDisposition,
    ProcedureReviewDisposition, ProcedureRun, ProcedureRunId, ProcedureScratchpad, ProcedureStage,
    ProcedureTask, ProcedureTerminalDisposition,
};

#[test]
fn procedure_run_round_trips_through_json() {
    let run = ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "add-procedure-localization-runner".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "Create typed procedure state".to_string(),
            covers: None,
        },
        spec_fingerprint: Some("spec-sha256".to_string()),
        repository_fingerprint: Some("repo-sha256".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad {
            goals: vec!["localize the change".to_string()],
            files: vec!["crates/deepseek-custom/src/procedure/run.rs".to_string()],
            changes: Vec::new(),
            last_error: None,
        },
        stage: ProcedureStage::Finished,
        attempts: vec![LocalizationAttempt {
            number: 1,
            backend: "local".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            disposition: ProcedureAttemptDisposition::Accepted,
            targets: vec![LocalizationTarget {
                path: "crates/deepseek-custom/src/procedure/run.rs".to_string(),
                symbol: Some("ProcedureRun".to_string()),
                evidence: "The task requires persistent run state.".to_string(),
            }],
            validation_error: None,
        }],
        review_disposition: ProcedureReviewDisposition::Pending,
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    };

    let json = serde_json::to_string(&run).expect("procedure run should serialize");
    let decoded: ProcedureRun =
        serde_json::from_str(&json).expect("procedure run should deserialize");

    assert_eq!(decoded, run);
}

#[test]
fn every_review_disposition_round_trips_exactly() {
    for disposition in [
        ProcedureReviewDisposition::Pending,
        ProcedureReviewDisposition::Approved,
        ProcedureReviewDisposition::Rejected,
        ProcedureReviewDisposition::LegacyUnreviewed,
    ] {
        let json = serde_json::to_string(&disposition).unwrap();
        let decoded: ProcedureReviewDisposition = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, disposition);
    }
}

#[test]
fn new_review_dispositions_default_to_pending() {
    assert_eq!(
        ProcedureReviewDisposition::default(),
        ProcedureReviewDisposition::Pending
    );
}
