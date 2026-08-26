use std::path::PathBuf;

use deepseek_custom::procedure::{
    BoundedVerifierOutput, CandidateEligibility, LocalizationAttempt, LocalizationTarget,
    ProcedureAttemptDisposition, ProcedureCandidateMetric, ProcedureGateOutcome,
    ProcedureReportStore, ProcedureReviewDisposition, ProcedureRun, ProcedureRunId,
    ProcedureScratchpad, ProcedureStage, ProcedureStageTiming, ProcedureTask,
    ProcedureTerminalDisposition, RouteSignal, RouteTier, VerifierCommandDisposition,
    VerifierCommandEvidence, VerifierGateDisposition, VerifierGateEvidence, VerifierReport,
};

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dsc-procedure-trace-export-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

fn sensitive_run() -> ProcedureRun {
    ProcedureRun {
        id: ProcedureRunId::new(),
        change_id: "change contains PROMPT-SECRET-923".to_string(),
        selected_task: ProcedureTask {
            id: "1.1".to_string(),
            text: "Prompt says PROMPT-SECRET-923 and source is SOURCE-CONTENT-441".to_string(),
            covers: Some("binding contains CREDENTIAL-KEY-782".to_string()),
        },
        spec_fingerprint: Some("fingerprint CREDENTIAL-KEY-782".to_string()),
        repository_fingerprint: Some("repository fingerprint SOURCE-CONTENT-441".to_string()),
        validation: None,
        scratchpad: ProcedureScratchpad {
            goals: vec!["PROMPT-SECRET-923".to_string()],
            files: vec!["SOURCE-CONTENT-441".to_string()],
            changes: vec!["CREDENTIAL-KEY-782".to_string()],
            last_error: Some("RAW-OUTPUT-556".to_string()),
        },
        stage: ProcedureStage::Finished,
        attempts: vec![
            LocalizationAttempt {
                number: 1,
                backend: "backend contains CREDENTIAL-KEY-782".to_string(),
                model: "model contains PROMPT-SECRET-923".to_string(),
                disposition: ProcedureAttemptDisposition::Accepted,
                targets: vec![
                    LocalizationTarget {
                        path: "src/zeta.rs".to_string(),
                        symbol: Some("Zeta".to_string()),
                        evidence: "SOURCE-CONTENT-441".to_string(),
                    },
                    LocalizationTarget {
                        path: "src/alpha.rs".to_string(),
                        symbol: None,
                        evidence: "PROMPT-SECRET-923".to_string(),
                    },
                ],
                validation_error: Some("RAW-OUTPUT-556".to_string()),
            },
            LocalizationAttempt {
                number: 2,
                backend: "another backend".to_string(),
                model: "another model".to_string(),
                disposition: ProcedureAttemptDisposition::Accepted,
                targets: vec![LocalizationTarget {
                    path: "src/zeta.rs".to_string(),
                    symbol: Some("Zeta".to_string()),
                    evidence: "duplicate SOURCE-CONTENT-441".to_string(),
                }],
                validation_error: None,
            },
        ],
        review_disposition: ProcedureReviewDisposition::Approved,
        terminal_disposition: Some(ProcedureTerminalDisposition::Succeeded),
    }
}

fn raw_output() -> BoundedVerifierOutput {
    BoundedVerifierOutput {
        text: "RAW-OUTPUT-556".to_string(),
        first_edge: "RAW-OUTPUT-556".to_string(),
        last_edge: "RAW-OUTPUT-556".to_string(),
        truncated: false,
        bytes_seen: 14,
    }
}

// covers: deepseek-custom/routing-sampling-and-metrics :: Exported localization traces protect workspace content :: Trace export is inspected
#[test]
fn localization_trace_export_uses_only_allowlisted_structured_fields() {
    let reports_dir = temp_path("redaction");
    let store = ProcedureReportStore::new(reports_dir.clone());
    let run = sensitive_run();
    store.save(&run).unwrap();

    let mut metrics = store
        .load_with_fingerprints(&run.id)
        .unwrap()
        .metrics
        .expect("terminal report has metrics");
    metrics.duration_ms = 91;
    metrics.stage_timings = vec![ProcedureStageTiming {
        stage: "stage name contains CREDENTIAL-KEY-782".to_string(),
        duration_ms: 17,
    }];
    metrics.route.selected_tier = Some(RouteTier::Local);
    metrics.route.signals = vec![RouteSignal::TargetCount(2)];
    metrics.route.local_mechanical_success = Some(true);
    metrics.route.escalation_triggers = vec!["RAW-OUTPUT-556".to_string()];
    metrics.candidates = vec![ProcedureCandidateMetric {
        index: 4,
        changed_line_count: Some(7),
        verifier_passed: Some(true),
    }];
    metrics.gate_outcomes = vec![ProcedureGateOutcome {
        gate: "gate name contains SOURCE-CONTENT-441".to_string(),
        passed: true,
    }];
    metrics.token_usage.input_tokens = Some(11);
    metrics.token_usage.output_tokens = Some(13);
    metrics.token_usage.total_tokens = Some(24);
    store.replace_metrics(&run.id, &metrics).unwrap();
    store
        .save_verification(
            &run.id,
            &VerifierReport {
                patch_gates: Vec::new(),
                gates: vec![VerifierGateEvidence {
                    command: "command contains CREDENTIAL-KEY-782".to_string(),
                    disposition: VerifierGateDisposition::Failed,
                    result: Some(VerifierCommandEvidence {
                        command: "command contains PROMPT-SECRET-923".to_string(),
                        disposition: VerifierCommandDisposition::Failed,
                        success: false,
                        exit_code: Some(1),
                        stdout: raw_output(),
                        stderr: raw_output(),
                        combined_output: raw_output(),
                        duration_millis: 42,
                        error: Some("RAW-OUTPUT-556".to_string()),
                    }),
                }],
                stopped_after_failure: true,
                first_failed_gate: Some(0),
                eligibility: CandidateEligibility::eligible(),
                terminal_disposition: None,
            },
        )
        .unwrap();

    let export = store.export_localization_traces().unwrap();
    let record = export.records.first().expect("one exported record");

    assert_eq!(
        record
            .targets
            .iter()
            .map(|target| (target.path.as_str(), target.symbol.as_deref()))
            .collect::<Vec<_>>(),
        vec![("src/alpha.rs", None), ("src/zeta.rs", Some("Zeta"))]
    );
    assert_eq!(record.route.selected_tier, Some(RouteTier::Local));
    assert_eq!(record.route.signals, vec![RouteSignal::TargetCount(2)]);
    assert!(record.route.escalated_to_frontier);
    assert_eq!(record.outcomes.localization_attempts.len(), 2);
    assert_eq!(record.outcomes.local_mechanical_success, Some(true));
    assert_eq!(
        record.outcomes.candidate_verifier_outcomes,
        vec![Some(true)]
    );
    assert_eq!(record.outcomes.gate_outcomes, vec![true]);
    assert_eq!(record.metrics.duration_ms, Some(91));
    assert_eq!(record.metrics.stage_durations_ms, vec![17]);
    assert_eq!(record.metrics.candidate_changed_line_counts, vec![Some(7)]);
    assert_eq!(record.metrics.input_tokens, Some(11));
    assert_eq!(record.metrics.output_tokens, Some(13));
    assert_eq!(record.metrics.total_tokens, Some(24));

    let json = export.to_pretty_json().unwrap();
    for excluded in [
        "PROMPT-SECRET-923",
        "CREDENTIAL-KEY-782",
        "SOURCE-CONTENT-441",
        "RAW-OUTPUT-556",
        "change_id",
        "scratchpad",
        "verification",
        "evidence",
        "command",
    ] {
        assert!(
            !json.contains(excluded),
            "trace export unexpectedly contains {excluded}: {json}"
        );
    }

    std::fs::remove_dir_all(reports_dir).ok();
}
