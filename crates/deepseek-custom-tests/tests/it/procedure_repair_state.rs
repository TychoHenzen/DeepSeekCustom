use std::collections::HashMap;

use deepseek_custom::config::settings::{BackendConfig, ProcedureSettings, Settings};
use deepseek_custom::procedure::{
    AttemptDisposition, AttemptFailureEvidence, AttemptState, ContractSelection, PatchPreview,
    PatchPreviewId, ProcedureInputFingerprints, ProcedureReviewDisposition, ProcedureRun,
    ProcedureRunId, ProcedureScratchpad, ProcedureStage, ProcedureTask,
    ProcedureTerminalDisposition, PromotionBaseline, ProposalScope, RepairCandidateId, RepairTier,
    RequirementSlice, RouteDecision, RouteOverride, RouteTier, SelectedContractSlice,
    StoredProcedureReport, ValidatedRepairInput,
};

fn candidate(value: &str) -> RepairCandidateId {
    RepairCandidateId::new(value).unwrap()
}

fn validated_input() -> ValidatedRepairInput {
    let run_id = ProcedureRunId::new();
    let task = ProcedureTask {
        id: "1.1".to_string(),
        text: "Repair the target".to_string(),
        covers: None,
    };
    ValidatedRepairInput {
        report: StoredProcedureReport {
            run: ProcedureRun {
                id: run_id,
                change_id: "change".to_string(),
                selected_task: task.clone(),
                spec_fingerprint: Some("spec".to_string()),
                repository_fingerprint: Some("repository".to_string()),
                validation: None,
                scratchpad: ProcedureScratchpad::default(),
                stage: ProcedureStage::Finished,
                attempts: Vec::new(),
                review_disposition: ProcedureReviewDisposition::Approved,
                terminal_disposition: Some(ProcedureTerminalDisposition::AwaitingReview),
            },
            input_fingerprints: ProcedureInputFingerprints::default(),
            verification: None,
        },
        contract: SelectedContractSlice {
            change_id: "change".to_string(),
            task,
            proposal_scope: ProposalScope {
                why: String::new(),
                what_changes: String::new(),
            },
            selection: ContractSelection::Bound {
                capability: "fixture".to_string(),
                requirement: RequirementSlice {
                    name: "Repair".to_string(),
                    text: "Repair the target.".to_string(),
                    scenarios: Vec::new(),
                },
            },
        },
        preview: PatchPreview {
            id: PatchPreviewId::new(),
            localization_run_id: run_id,
            change_id: "change".to_string(),
            task_id: "1.1".to_string(),
            route: RouteDecision {
                automatic_tier: RouteTier::Local,
                effective_tier: RouteTier::Local,
                signals: Vec::new(),
                selected_override: RouteOverride::Automatic,
                overridden: false,
            },
            backend: "local".to_string(),
            model: "local-model".to_string(),
            targets: vec!["src/lib.rs".to_string()],
            rationale: "fixture".to_string(),
            unified_diff: "fixture".to_string(),
        },
        promotion_baseline: PromotionBaseline::from_fingerprints(Vec::new()),
    }
}

fn policy(
    structural_retries: u8,
    local_verifier_attempts: u8,
    frontier_attempts: u8,
) -> deepseek_custom::config::settings::ValidatedProcedureRepairPolicy {
    let frontier_backend = (frontier_attempts > 0).then(|| "frontier".to_string());
    let backends = frontier_backend.as_ref().map(|name| {
        HashMap::from([(
            name.clone(),
            BackendConfig::CodexCli {
                model: "gpt-5".to_string(),
                sandbox: Some("workspace-write".to_string()),
                env: None,
                models: None,
            },
        )])
    });
    Settings {
        procedure: Some(ProcedureSettings {
            frontier_patch_backend: frontier_backend,
            structural_retries,
            local_verifier_attempts,
            frontier_attempts,
            ..ProcedureSettings::default()
        }),
        backends,
        ..Settings::default()
    }
    .validated_procedure_repair_policy()
    .unwrap()
}

fn state(
    structural_retries: u8,
    local_verifier_attempts: u8,
    frontier_attempts: u8,
) -> AttemptState {
    AttemptState::from_validated_input(
        &validated_input(),
        policy(
            structural_retries,
            local_verifier_attempts,
            frontier_attempts,
        ),
    )
}

#[test]
fn validated_input_creates_one_based_local_ready_state() {
    let state = state(1, 3, 2);

    assert_eq!(state.tier(), RepairTier::Local);
    assert_eq!(state.attempt_index(), 1);
    assert_eq!(state.structural_retry_count(), 0);
    assert_eq!(state.last_candidate(), None);
    assert!(state.failures().is_empty());
    assert_eq!(state.disposition(), &AttemptDisposition::Ready);
    assert_eq!(state.policy().local_verifier_attempts(), 3);
}

#[test]
fn explicit_transitions_move_from_local_through_frontier_to_promotion() {
    let mut state = state(1, 1, 1);
    state.start_local_candidate(candidate("local-1")).unwrap();
    state
        .retry_structural(
            AttemptFailureEvidence::structural("invalid patch envelope"),
            candidate("local-1-structural-retry"),
        )
        .unwrap();
    state
        .local_verifier_failure(AttemptFailureEvidence::verifier(
            "cargo test",
            Some(101),
            "test failed",
        ))
        .unwrap();
    assert_eq!(state.disposition(), &AttemptDisposition::LocalExhausted);

    state.escalate_to_frontier(candidate("frontier-1")).unwrap();
    assert_eq!(state.tier(), RepairTier::Frontier);
    assert_eq!(state.attempt_index(), 1);
    assert_eq!(state.disposition(), &AttemptDisposition::CandidateActive);
    state.promote().unwrap();
    assert_eq!(state.disposition(), &AttemptDisposition::Promoted);
    assert!(state.disposition().is_terminal());
}

#[test]
fn every_terminal_disposition_rejects_local_and_frontier_dispatch() {
    let terminal_states = [
        {
            let mut state = state(1, 1, 1);
            state.start_local_candidate(candidate("promoted")).unwrap();
            state.promote().unwrap();
            state
        },
        {
            let mut state = state(1, 1, 1);
            state.block("human action").unwrap();
            state
        },
        {
            let mut state = state(1, 1, 1);
            state.fail("infrastructure").unwrap();
            state
        },
        {
            let mut state = state(1, 1, 1);
            state.interrupt().unwrap();
            state
        },
    ];

    for mut state in terminal_states {
        let disposition = state.disposition().name();
        assert_eq!(
            state
                .start_local_candidate(candidate("later-local"))
                .unwrap_err()
                .to_string(),
            format!("cannot dispatch local candidate from terminal disposition `{disposition}`")
        );
        assert_eq!(
            state
                .escalate_to_frontier(candidate("later-frontier"))
                .unwrap_err()
                .to_string(),
            format!("cannot dispatch frontier candidate from terminal disposition `{disposition}`")
        );
    }
}

fn verifier_failure(label: &str) -> AttemptFailureEvidence {
    AttemptFailureEvidence::verifier(
        format!("verify-{label}"),
        Some(10),
        format!("{label} failed"),
    )
}

#[test]
fn local_and_frontier_transition_table_pins_every_state_field() {
    let mut state = state(1, 2, 2);
    let transitions = [
        (
            "local candidate start",
            RepairTier::Local,
            1,
            0,
            "local-1",
            0,
            AttemptDisposition::CandidateActive,
        ),
        (
            "structural retry",
            RepairTier::Local,
            1,
            1,
            "local-1-retry",
            1,
            AttemptDisposition::CandidateActive,
        ),
        (
            "local verifier retry",
            RepairTier::Local,
            2,
            1,
            "local-1-retry",
            2,
            AttemptDisposition::Ready,
        ),
        (
            "second local candidate",
            RepairTier::Local,
            2,
            1,
            "local-2",
            2,
            AttemptDisposition::CandidateActive,
        ),
        (
            "local exhaustion",
            RepairTier::Local,
            2,
            1,
            "local-2",
            3,
            AttemptDisposition::LocalExhausted,
        ),
        (
            "frontier escalation",
            RepairTier::Frontier,
            1,
            1,
            "frontier-1",
            3,
            AttemptDisposition::CandidateActive,
        ),
        (
            "frontier verifier retry",
            RepairTier::Frontier,
            2,
            1,
            "frontier-1",
            4,
            AttemptDisposition::Ready,
        ),
        (
            "second frontier candidate",
            RepairTier::Frontier,
            2,
            1,
            "frontier-2",
            4,
            AttemptDisposition::CandidateActive,
        ),
        (
            "frontier exhaustion",
            RepairTier::Frontier,
            2,
            1,
            "frontier-2",
            5,
            AttemptDisposition::FrontierExhausted,
        ),
    ];

    for (index, expected) in transitions.into_iter().enumerate() {
        match index {
            0 => state.start_local_candidate(candidate("local-1")).unwrap(),
            1 => state
                .retry_structural(
                    AttemptFailureEvidence::structural("bad envelope"),
                    candidate("local-1-retry"),
                )
                .unwrap(),
            2 => state
                .local_verifier_failure(verifier_failure("local-1-retry"))
                .unwrap(),
            3 => state.start_local_candidate(candidate("local-2")).unwrap(),
            4 => state
                .local_verifier_failure(verifier_failure("local-2"))
                .unwrap(),
            5 => state.escalate_to_frontier(candidate("frontier-1")).unwrap(),
            6 => state
                .frontier_verifier_failure(verifier_failure("frontier-1"))
                .unwrap(),
            7 => state.escalate_to_frontier(candidate("frontier-2")).unwrap(),
            8 => state
                .frontier_verifier_failure(verifier_failure("frontier-2"))
                .unwrap(),
            _ => unreachable!(),
        }

        let (
            name,
            expected_tier,
            expected_attempt,
            expected_structural_retries,
            expected_candidate,
            expected_failures,
            expected_disposition,
        ) = expected;
        assert_eq!(state.tier(), expected_tier, "{name}");
        assert_eq!(state.attempt_index(), expected_attempt, "{name}");
        assert_eq!(
            state.structural_retry_count(),
            expected_structural_retries,
            "{name}"
        );
        assert_eq!(
            state.last_candidate().map(RepairCandidateId::as_str),
            Some(expected_candidate),
            "{name}"
        );
        assert_eq!(state.failures().len(), expected_failures, "{name}");
        assert_eq!(state.disposition(), &expected_disposition, "{name}");
    }

    let failures = state.failures();
    assert_eq!(failures[0].tier(), RepairTier::Local);
    assert_eq!(failures[0].attempt_index(), 1);
    assert_eq!(failures[0].candidate().as_str(), "local-1");
    assert_eq!(failures[0].evidence().diagnostic(), "bad envelope");
    assert_eq!(failures[1].candidate().as_str(), "local-1-retry");
    assert_eq!(
        failures[1].evidence().command(),
        Some("verify-local-1-retry")
    );
    assert_eq!(failures[1].evidence().exit_code(), Some(10));
    assert_eq!(failures[3].tier(), RepairTier::Frontier);
    assert_eq!(failures[3].attempt_index(), 1);
    assert_eq!(failures[4].attempt_index(), 2);
}

#[test]
fn local_budget_table_exhausts_at_default_and_maximum_without_an_extra_dispatch() {
    for (name, budget) in [("default", 3_u8), ("maximum", 4_u8)] {
        let mut state = state(1, budget, 0);
        for attempt in 1..=budget {
            state
                .start_local_candidate(candidate(&format!("local-{attempt}")))
                .unwrap();
            state
                .local_verifier_failure(verifier_failure(&format!("local-{attempt}")))
                .unwrap();
            let expected = if attempt == budget {
                AttemptDisposition::LocalExhausted
            } else {
                AttemptDisposition::Ready
            };
            assert_eq!(
                state.attempt_index(),
                if attempt == budget {
                    attempt
                } else {
                    attempt + 1
                },
                "{name} attempt {attempt}"
            );
            assert_eq!(state.disposition(), &expected, "{name} attempt {attempt}");
        }

        let exhausted = state.clone();
        assert_eq!(
            state
                .start_local_candidate(candidate("unlisted-extra-local"))
                .unwrap_err()
                .to_string(),
            format!(
                "attempt transition `start_local_candidate` is not allowed from disposition `local_exhausted` at local attempt {budget}"
            ),
            "{name}"
        );
        assert_eq!(state, exhausted, "{name}");
        assert_eq!(state.failures().len(), usize::from(budget), "{name}");
    }
}

#[test]
fn frontier_budget_table_exhausts_at_every_enabled_limit_without_a_third_dispatch() {
    for (name, budget) in [("downward", 1_u8), ("default-and-maximum", 2_u8)] {
        let mut state = state(0, 1, budget);
        state.start_local_candidate(candidate("local-1")).unwrap();
        state
            .local_verifier_failure(verifier_failure("local-1"))
            .unwrap();
        state.escalate_to_frontier(candidate("frontier-1")).unwrap();

        for attempt in 1..=budget {
            if attempt > 1 {
                state
                    .escalate_to_frontier(candidate(&format!("frontier-{attempt}")))
                    .unwrap();
            }
            state
                .frontier_verifier_failure(verifier_failure(&format!("frontier-{attempt}")))
                .unwrap();
            let expected = if attempt == budget {
                AttemptDisposition::FrontierExhausted
            } else {
                AttemptDisposition::Ready
            };
            assert_eq!(
                state.attempt_index(),
                if attempt == budget {
                    attempt
                } else {
                    attempt + 1
                },
                "{name} attempt {attempt}"
            );
            assert_eq!(state.disposition(), &expected, "{name} attempt {attempt}");
        }

        let exhausted = state.clone();
        assert_eq!(
            state
                .escalate_to_frontier(candidate("unlisted-extra-frontier"))
                .unwrap_err()
                .to_string(),
            format!(
                "attempt transition `escalate_to_frontier` is not allowed from disposition `frontier_exhausted` at frontier attempt {budget}"
            ),
            "{name}"
        );
        assert_eq!(state, exhausted, "{name}");
    }
}

#[test]
fn promotion_table_accepts_only_active_local_or_frontier_candidates() {
    for tier in [RepairTier::Local, RepairTier::Frontier] {
        let mut state = state(0, 1, 1);
        state.start_local_candidate(candidate("local-1")).unwrap();
        if tier == RepairTier::Frontier {
            state
                .local_verifier_failure(verifier_failure("local-1"))
                .unwrap();
            state.escalate_to_frontier(candidate("frontier-1")).unwrap();
        }

        state.promote().unwrap();
        assert_eq!(state.tier(), tier);
        assert_eq!(state.disposition(), &AttemptDisposition::Promoted);
        assert!(state.disposition().is_terminal());
    }
}

#[derive(Debug, Clone, Copy)]
enum TerminalCase {
    Promoted,
    Blocked,
    Failed,
    Interrupted,
}

fn terminal_state(case: TerminalCase) -> AttemptState {
    let mut state = state(1, 1, 1);
    match case {
        TerminalCase::Promoted => {
            state.start_local_candidate(candidate("candidate")).unwrap();
            state.promote().unwrap();
        }
        TerminalCase::Blocked => state.block("human action required").unwrap(),
        TerminalCase::Failed => state.fail("infrastructure failed").unwrap(),
        TerminalCase::Interrupted => state.interrupt().unwrap(),
    }
    state
}

#[test]
fn terminal_state_table_rejects_every_dispatch_transition_without_mutation() {
    for terminal in [
        TerminalCase::Promoted,
        TerminalCase::Blocked,
        TerminalCase::Failed,
        TerminalCase::Interrupted,
    ] {
        let mut state = terminal_state(terminal);
        let expected = state.clone();
        let disposition = state.disposition().name();
        let dispatch_errors = [
            state
                .start_local_candidate(candidate("later-local"))
                .unwrap_err()
                .to_string(),
            state
                .retry_structural(
                    AttemptFailureEvidence::structural("later structural"),
                    candidate("later-structural"),
                )
                .unwrap_err()
                .to_string(),
            state
                .escalate_to_frontier(candidate("later-escalation"))
                .unwrap_err()
                .to_string(),
        ];
        assert_eq!(
            dispatch_errors,
            [
                format!(
                    "cannot dispatch local candidate from terminal disposition `{disposition}`"
                ),
                format!(
                    "cannot dispatch local candidate from terminal disposition `{disposition}`"
                ),
                format!(
                    "cannot dispatch frontier candidate from terminal disposition `{disposition}`"
                ),
            ],
            "{terminal:?}"
        );
        assert_eq!(state, expected, "{terminal:?}");
    }
}

#[derive(Debug, Clone, Copy)]
enum IllegalCase {
    StructuralBeforeCandidate,
    LocalFailureBeforeCandidate,
    DuplicateLocalStart,
    EscalateBeforeExhaustion,
    FrontierFailureWhileLocal,
    PromoteWithoutCandidate,
    WrongStructuralEvidence,
    StructuralBudgetExhausted,
    DisabledEscalation,
}

#[test]
fn illegal_transition_table_has_exact_diagnostics_and_no_fallthrough_mutation() {
    let cases = [
        (
            IllegalCase::StructuralBeforeCandidate,
            "attempt transition `retry_structural` is not allowed from disposition `ready` at local attempt 1",
        ),
        (
            IllegalCase::LocalFailureBeforeCandidate,
            "attempt transition `local_verifier_failure` is not allowed from disposition `ready` at local attempt 1",
        ),
        (
            IllegalCase::DuplicateLocalStart,
            "attempt transition `start_local_candidate` is not allowed from disposition `candidate_active` at local attempt 1",
        ),
        (
            IllegalCase::EscalateBeforeExhaustion,
            "attempt transition `escalate_to_frontier` is not allowed from disposition `ready` at local attempt 1",
        ),
        (
            IllegalCase::FrontierFailureWhileLocal,
            "attempt transition `frontier_verifier_failure` is not allowed from disposition `candidate_active` at local attempt 1",
        ),
        (
            IllegalCase::PromoteWithoutCandidate,
            "attempt transition `promote` is not allowed from disposition `ready` at local attempt 1",
        ),
        (
            IllegalCase::WrongStructuralEvidence,
            "attempt transition `retry_structural` requires structural failure evidence, got verifier",
        ),
        (
            IllegalCase::StructuralBudgetExhausted,
            "attempt transition `retry_structural` exhausted structural retry budget 1",
        ),
        (
            IllegalCase::DisabledEscalation,
            "frontier escalation is disabled by procedure.frontier_attempts = 0",
        ),
    ];

    for (case, expected_error) in cases {
        let mut state = match case {
            IllegalCase::DisabledEscalation => state(0, 1, 0),
            _ => state(1, 1, 1),
        };
        match case {
            IllegalCase::DuplicateLocalStart
            | IllegalCase::FrontierFailureWhileLocal
            | IllegalCase::WrongStructuralEvidence
            | IllegalCase::StructuralBudgetExhausted => {
                state.start_local_candidate(candidate("local-1")).unwrap();
            }
            IllegalCase::DisabledEscalation => {
                state.start_local_candidate(candidate("local-1")).unwrap();
                state
                    .local_verifier_failure(verifier_failure("local-1"))
                    .unwrap();
            }
            _ => {}
        }
        if matches!(case, IllegalCase::StructuralBudgetExhausted) {
            state
                .retry_structural(
                    AttemptFailureEvidence::structural("first structural failure"),
                    candidate("local-1-retry"),
                )
                .unwrap();
        }
        let before = state.clone();

        let error = match case {
            IllegalCase::StructuralBeforeCandidate | IllegalCase::StructuralBudgetExhausted => {
                state.retry_structural(
                    AttemptFailureEvidence::structural("structural failure"),
                    candidate("retry"),
                )
            }
            IllegalCase::LocalFailureBeforeCandidate => {
                state.local_verifier_failure(verifier_failure("local"))
            }
            IllegalCase::DuplicateLocalStart => state.start_local_candidate(candidate("duplicate")),
            IllegalCase::EscalateBeforeExhaustion | IllegalCase::DisabledEscalation => {
                state.escalate_to_frontier(candidate("frontier"))
            }
            IllegalCase::FrontierFailureWhileLocal => {
                state.frontier_verifier_failure(verifier_failure("frontier"))
            }
            IllegalCase::PromoteWithoutCandidate => state.promote(),
            IllegalCase::WrongStructuralEvidence => {
                state.retry_structural(verifier_failure("wrong-kind"), candidate("retry"))
            }
        }
        .unwrap_err();

        assert_eq!(error.to_string(), expected_error, "{case:?}");
        assert_eq!(state, before, "{case:?}");
    }
}

#[test]
fn terminal_transition_table_covers_every_nonterminal_source() {
    #[derive(Debug, Clone, Copy)]
    enum Source {
        LocalReady,
        LocalActive,
        LocalExhausted,
        FrontierActive,
        FrontierReady,
        FrontierExhausted,
    }

    fn source_state(source: Source) -> AttemptState {
        let mut state = state(1, 1, 2);
        match source {
            Source::LocalReady => {}
            Source::LocalActive => state.start_local_candidate(candidate("local-1")).unwrap(),
            Source::LocalExhausted => {
                state.start_local_candidate(candidate("local-1")).unwrap();
                state
                    .local_verifier_failure(verifier_failure("local-1"))
                    .unwrap();
            }
            Source::FrontierActive | Source::FrontierReady | Source::FrontierExhausted => {
                state.start_local_candidate(candidate("local-1")).unwrap();
                state
                    .local_verifier_failure(verifier_failure("local-1"))
                    .unwrap();
                state.escalate_to_frontier(candidate("frontier-1")).unwrap();
                if matches!(source, Source::FrontierReady | Source::FrontierExhausted) {
                    state
                        .frontier_verifier_failure(verifier_failure("frontier-1"))
                        .unwrap();
                }
                if matches!(source, Source::FrontierExhausted) {
                    state.escalate_to_frontier(candidate("frontier-2")).unwrap();
                    state
                        .frontier_verifier_failure(verifier_failure("frontier-2"))
                        .unwrap();
                }
            }
        }
        state
    }

    for source in [
        Source::LocalReady,
        Source::LocalActive,
        Source::LocalExhausted,
        Source::FrontierActive,
        Source::FrontierReady,
        Source::FrontierExhausted,
    ] {
        let original = source_state(source);
        for terminal in [
            TerminalCase::Blocked,
            TerminalCase::Failed,
            TerminalCase::Interrupted,
        ] {
            let mut state = original.clone();
            let expected = match terminal {
                TerminalCase::Blocked => {
                    state.block("human action required").unwrap();
                    AttemptDisposition::Blocked {
                        reason: "human action required".to_string(),
                    }
                }
                TerminalCase::Failed => {
                    state.fail("infrastructure failed").unwrap();
                    AttemptDisposition::Failed {
                        reason: "infrastructure failed".to_string(),
                    }
                }
                TerminalCase::Interrupted => {
                    state.interrupt().unwrap();
                    AttemptDisposition::Interrupted
                }
                TerminalCase::Promoted => unreachable!(),
            };
            assert_eq!(state.disposition(), &expected, "{source:?} {terminal:?}");
            assert!(state.disposition().is_terminal(), "{source:?} {terminal:?}");
            assert_eq!(state.tier(), original.tier(), "{source:?} {terminal:?}");
            assert_eq!(
                state.attempt_index(),
                original.attempt_index(),
                "{source:?} {terminal:?}"
            );
            assert_eq!(
                state.structural_retry_count(),
                original.structural_retry_count(),
                "{source:?} {terminal:?}"
            );
            assert_eq!(
                state.last_candidate(),
                original.last_candidate(),
                "{source:?} {terminal:?}"
            );
            assert_eq!(
                state.failures(),
                original.failures(),
                "{source:?} {terminal:?}"
            );
        }
    }
}

#[test]
fn candidate_identity_rejects_blank_values() {
    assert_eq!(
        RepairCandidateId::new("  ").unwrap_err().to_string(),
        "repair candidate identity must not be blank"
    );
}
