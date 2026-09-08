//! Integration coverage for bounded recovery identity, diagnosis, and retry.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use deepseek_custom::backend::factory::BackendFactory;
use deepseek_custom::backend::registry::SubagentRegistry;
use deepseek_custom::backend::stub::StubTurn;
use deepseek_custom::config::settings::Settings;
use deepseek_custom::recovery::RetryClaim;
use deepseek_custom::recovery::{
    AttemptStatus, DiagnosisOutcome, DiagnosticResponse, FailureContext, IdentityError,
    IdentityInput, MAX_PERMITTED_RETRIES, ProjectReference, RecoveryCoordinator, RecoveryRun,
    RecoveryStatus, RecoveryStore, RepositoryProvider, SafeRetryAction, SafeRetrySpec,
    WorkItemKind, parse_diagnostic_response, render_diagnostic_prompt, resolve_identity,
    sanitize_text,
};
use tokio::sync::Notify;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("dsc-recovery-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn github_input(item_reference: &str) -> IdentityInput {
    IdentityInput {
        repository_reference: Some("https://github.com/TychoHenzen/DeepSeekCustom.git".to_string()),
        checkout_remote: Some("git@github.com:tychohenzen/deepseekcustom.git".to_string()),
        project: Some(ProjectReference::github("TychoHenzen/6")),
        item_reference: Some(item_reference.to_string()),
    }
}

fn diagnostic_factory(script: Vec<StubTurn>) -> Arc<BackendFactory> {
    Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from("."))
            .with_stub("diagnostic", script),
    )
}

fn coordinator(factory: Arc<BackendFactory>, directory: PathBuf) -> RecoveryCoordinator {
    RecoveryCoordinator::new(factory, RecoveryStore::new(directory), "diagnostic", None)
}

fn retry_spec() -> SafeRetrySpec {
    SafeRetrySpec::new("safe-reset", "repeat the same safe verification step").unwrap()
}

fn response_json(outcome: &str, extra: &str) -> String {
    format!(
        r#"{{"outcome":"{outcome}","summary":"diagnostic summary","evidence":["evidence one"],{extra}}}"#
    )
}

#[test]
fn equivalent_repository_and_pull_request_references_resolve_once() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();

    assert_eq!(identity.provider, RepositoryProvider::GitHub);
    assert_eq!(identity.repository.namespace, "tychohenzen");
    assert_eq!(identity.repository.name, "deepseekcustom");
    assert_eq!(identity.project.key, "tychohenzen/6");
    assert_eq!(identity.item.kind, WorkItemKind::PullRequest);
    assert_eq!(identity.item.number, 42);
}

#[test]
fn checkout_remote_can_supply_the_repository_identity() {
    let mut input = github_input("https://github.com/tychohenzen/deepseekcustom/pull/42");
    input.repository_reference = None;

    let identity = resolve_identity(&input).unwrap();
    assert_eq!(identity.repository.name, "deepseekcustom");
}

#[test]
fn azure_devops_repository_and_pull_request_references_share_one_identity() {
    let input = IdentityInput {
        repository_reference: Some(
            "https://dev.azure.com/acme/engineering/_git/deepseekcustom".to_string(),
        ),
        checkout_remote: Some(
            "https://acme@dev.azure.com/acme/engineering/_git/deepseekcustom".to_string(),
        ),
        project: Some(ProjectReference::azure_devops("acme/engineering")),
        item_reference: Some(
            "https://dev.azure.com/acme/engineering/_git/deepseekcustom/pullrequest/7".to_string(),
        ),
    };

    let identity = resolve_identity(&input).unwrap();
    assert_eq!(identity.provider, RepositoryProvider::AzureDevOps);
    assert_eq!(identity.repository.namespace, "acme");
    assert_eq!(identity.repository.name, "deepseekcustom");
    assert_eq!(identity.item.kind, WorkItemKind::PullRequest);
    assert_eq!(identity.item.number, 7);
}

#[test]
fn azure_devops_ssh_checkout_remote_resolves_repository_identity() {
    let input = IdentityInput {
        repository_reference: None,
        checkout_remote: Some(
            "git@ssh.dev.azure.com:v3/acme/engineering/deepseekcustom".to_string(),
        ),
        project: Some(ProjectReference::azure_devops("acme/engineering")),
        item_reference: Some(
            "https://dev.azure.com/acme/engineering/_git/deepseekcustom/pullrequest/7".to_string(),
        ),
    };

    let identity = resolve_identity(&input).unwrap();
    assert_eq!(identity.provider, RepositoryProvider::AzureDevOps);
    assert_eq!(identity.repository.namespace, "acme");
    assert_eq!(identity.repository.name, "deepseekcustom");
    assert_eq!(identity.item.number, 7);
}

#[test]
fn azure_devops_project_mismatch_is_rejected_even_when_repository_names_match() {
    let input = IdentityInput {
        repository_reference: None,
        checkout_remote: Some("git@ssh.dev.azure.com:v3/acme/project-a/deepseekcustom".to_string()),
        project: Some(ProjectReference::azure_devops("acme/project-a")),
        item_reference: Some(
            "https://dev.azure.com/acme/project-b/_git/deepseekcustom/pullrequest/7".to_string(),
        ),
    };

    assert!(matches!(
        resolve_identity(&input),
        Err(IdentityError::ItemRepositoryMismatch)
    ));
}

#[test]
fn equivalent_azure_project_keys_are_canonicalized() {
    let mut short_project = IdentityInput {
        repository_reference: Some(
            "https://dev.azure.com/acme/engineering/_git/deepseekcustom".to_string(),
        ),
        checkout_remote: None,
        project: Some(ProjectReference::azure_devops("engineering")),
        item_reference: Some(
            "https://dev.azure.com/acme/engineering/_git/deepseekcustom/pullrequest/7".to_string(),
        ),
    };
    let full_project = IdentityInput {
        project: Some(ProjectReference::azure_devops("acme/engineering")),
        ..short_project.clone()
    };

    let short = resolve_identity(&short_project).unwrap();
    let full = resolve_identity(&full_project).unwrap();
    assert_eq!(short.project, full.project);
    short_project.project = Some(ProjectReference::azure_devops("acme/other"));
    assert!(resolve_identity(&short_project).is_err());
}

#[test]
fn repository_provider_mismatch_is_rejected() {
    let mut input = github_input("https://github.com/tychohenzen/deepseekcustom/pull/42");
    input.checkout_remote =
        Some("https://dev.azure.com/acme/engineering/_git/deepseekcustom".to_string());

    let error = resolve_identity(&input).unwrap_err();
    assert!(matches!(error, IdentityError::ProviderMismatch { .. }));
}

#[test]
fn ambiguous_pull_request_reference_is_rejected_without_guessing() {
    let mut input = github_input("PR 42");
    input.repository_reference = None;
    input.checkout_remote = Some("git@github.com:TychoHenzen/DeepSeekCustom.git".to_string());

    let error = resolve_identity(&input).unwrap_err();
    assert!(matches!(error, IdentityError::AmbiguousItemReference(_)));
}

#[test]
fn diagnostic_input_redacts_credentials_and_excludes_unrelated_context() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let failure = FailureContext::new(
        "verify",
        "Authorization: Bearer ghp_not-for-the-model token=private-token C:\\secret\\file",
        &["api_key=sk-private".to_string()],
    );
    let prompt = render_diagnostic_prompt(&identity, &failure, &[retry_spec()]);

    assert!(prompt.contains("[REDACTED]"));
    assert!(!prompt.contains("ghp_not-for-the-model"));
    assert!(!prompt.contains("sk-private"));
    assert!(!prompt.contains("C:\\secret\\file"));
    assert!(prompt.contains("current_step: verify"));
    assert!(prompt.contains("safe-reset"));
}

#[test]
fn diagnostic_json_requires_a_bounded_classification() {
    let parsed = parse_diagnostic_response(&response_json(
        "retryable",
        r#""retry_key":"safe-reset","question":null,"requested_action":null"#,
    ))
    .unwrap();
    assert_eq!(parsed.outcome, DiagnosisOutcome::Retryable);
    assert_eq!(parsed.retry_key.as_deref(), Some("safe-reset"));

    let error = parse_diagnostic_response(r#"{"outcome":"retryable","summary":"missing key"}"#)
        .unwrap_err();
    assert!(error.contains("retry_key"));

    let parsed = parse_diagnostic_response(
        r#"{"outcome":"needs_decision","summary":"missing fact","question":"Which fact is missing?","requested_action":null}"#,
    )
    .unwrap();
    assert!(parsed.requested_action.is_some());
}

#[test]
fn recovery_step_is_sanitized_once_and_remains_diagnosable() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let raw_step = "token=step-secret";
    let mut run = RecoveryRun::new(identity, raw_step, 1);

    assert!(!run.record().current_step.contains("step-secret"));
    run.begin_diagnosis(FailureContext::new(raw_step, "failure", &[]), Vec::new())
        .unwrap();
}

#[test]
fn permitted_retry_choices_have_a_total_bound() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let mut run = RecoveryRun::new(identity, "verify", 1);
    let retries = (0..=MAX_PERMITTED_RETRIES)
        .map(|index| SafeRetrySpec::new(format!("safe-{index}"), "safe action").unwrap())
        .collect();

    let error = run
        .begin_diagnosis(FailureContext::new("verify", "failure", &[]), retries)
        .unwrap_err();
    assert!(error.to_string().contains("permitted retry count"));
}

struct CountingAction {
    calls: AtomicUsize,
    fail: AtomicBool,
}

#[async_trait]
impl SafeRetryAction for CountingAction {
    async fn retry(&self, claim: &RetryClaim) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            Err(format!("retry failed for {}", claim.key))
        } else {
            Ok(format!("retry {} completed", claim.key))
        }
    }
}

struct BlockingAction {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl SafeRetryAction for BlockingAction {
    async fn retry(&self, claim: &RetryClaim) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(format!("retry {} completed", claim.key))
    }
}

#[tokio::test]
async fn diagnostic_session_closes_and_resolved_state_is_persisted() {
    let directory = temp_dir("resolved");
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "resolved",
        r#""retry_key":null,"question":null,"requested_action":null"#,
    ))]);
    let coordinator = coordinator(factory, directory.clone());
    let mut run = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();

    let status = coordinator
        .diagnose(
            &mut run,
            "command failed with token=hidden",
            &[],
            vec![retry_spec()],
        )
        .await
        .unwrap();

    assert_eq!(status, RecoveryStatus::Resolved);
    assert_eq!(coordinator.registry().len().await, 0);
    let loaded = coordinator.load_run(&run.id()).unwrap();
    assert_eq!(loaded.status(), RecoveryStatus::Resolved);
    assert!(
        loaded
            .record()
            .last_diagnostic
            .as_ref()
            .unwrap()
            .session_closed
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn retryable_diagnosis_executes_one_authorized_retry_and_deduplicates_it() {
    let directory = temp_dir("retryable");
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "retryable",
        r#""retry_key":"safe-reset","question":null,"requested_action":null"#,
    ))]);
    let coordinator = coordinator(factory, directory.clone());
    let mut run = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();
    coordinator
        .diagnose(&mut run, "verification failed", &[], vec![retry_spec()])
        .await
        .unwrap();

    let action = CountingAction {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    };
    let result = coordinator.execute_retry(&mut run, &action).await.unwrap();
    assert!(matches!(
        result,
        deepseek_custom::recovery::RetryResult::Succeeded { .. }
    ));
    assert_eq!(action.calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.status(), RecoveryStatus::Ready);
    assert_eq!(
        run.record().attempted_actions[0].status,
        AttemptStatus::Succeeded
    );

    let duplicate = coordinator
        .execute_retry(&mut run, &action)
        .await
        .unwrap_err();
    assert!(duplicate.to_string().contains("not waiting"));
    assert_eq!(action.calls.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn durable_run_owners_cannot_claim_the_same_retry_concurrently() {
    let directory = temp_dir("retry-lock");
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "retryable",
        r#""retry_key":"safe-reset","question":null,"requested_action":null"#,
    ))]);
    let coordinator_one = coordinator(Arc::clone(&factory), directory.clone());
    let coordinator_two = coordinator(Arc::clone(&factory), directory.clone());
    let mut run_one = coordinator_one
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();
    coordinator_one
        .diagnose(&mut run_one, "verification failed", &[], vec![retry_spec()])
        .await
        .unwrap();
    let mut run_two = coordinator_two.load_run(&run_one.id()).unwrap();

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let first_action = BlockingAction {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
        calls: Arc::clone(&calls),
    };
    let first_task = tokio::spawn(async move {
        coordinator_one
            .execute_retry(&mut run_one, &first_action)
            .await
    });
    started.notified().await;

    let second_action = CountingAction {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    };
    let second_result = coordinator_two
        .execute_retry(&mut run_two, &second_action)
        .await
        .unwrap_err();
    assert!(second_result.to_string().contains("already claimed"));
    assert_eq!(second_action.calls.load(Ordering::SeqCst), 0);

    release.notify_one();
    first_task.await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn blocked_and_needs_decision_evidence_survives_reload() {
    let directory = temp_dir("blocked");
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "blocked",
        r#""retry_key":null,"question":"Which provider owns this item?","requested_action":"Confirm the provider identity""#,
    ))]);
    let coordinator = coordinator(factory, directory.clone());
    let mut run = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "resolve",
            1,
        )
        .unwrap();

    let status = coordinator
        .diagnose(
            &mut run,
            "provider returned an ambiguous item reference",
            &["checkout remote was inspected".to_string()],
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(status, RecoveryStatus::Blocked);

    let restored = coordinator.load_run(&run.id()).unwrap();
    assert_eq!(restored.status(), RecoveryStatus::Blocked);
    assert_eq!(
        restored.record().question.as_deref(),
        Some("Which provider owns this item?")
    );
    assert_eq!(
        restored.record().next_required_decision.as_deref(),
        Some("Confirm the provider identity")
    );
    assert!(
        restored
            .record()
            .evidence
            .iter()
            .any(|evidence| evidence.detail == "evidence one")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn needs_decision_diagnostic_is_retained_and_closes_its_session() {
    let directory = temp_dir("needs-decision");
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "needs_decision",
        r#""retry_key":null,"question":"Which authorization applies?","requested_action":"Choose the permitted scope""#,
    ))]);
    let coordinator = coordinator(factory, directory.clone());
    let mut run = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "resolve",
            1,
        )
        .unwrap();

    let status = coordinator
        .diagnose(
            &mut run,
            "authorization boundary is unclear",
            &[],
            Vec::new(),
        )
        .await
        .unwrap();

    assert_eq!(status, RecoveryStatus::NeedsDecision);
    assert_eq!(coordinator.registry().len().await, 0);
    assert_eq!(
        run.record().question.as_deref(),
        Some("Which authorization applies?")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn duplicate_and_cross_project_results_are_rejected_without_mutation() {
    let factory = diagnostic_factory(vec![StubTurn::Text(response_json(
        "resolved",
        r#""retry_key":null,"question":null,"requested_action":null"#,
    ))]);
    let registry = Arc::new(SubagentRegistry::new());
    let directory = temp_dir("stale");
    let coordinator = RecoveryCoordinator::with_registry(
        factory,
        RecoveryStore::new(directory.clone()),
        "diagnostic",
        None,
        registry,
    );
    let mut first = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();
    let context = FailureContext::new("verify", "failure", &[]);
    let token = first.begin_diagnosis(context, Vec::new()).unwrap();
    let response = DiagnosticResponse {
        outcome: DiagnosisOutcome::Resolved,
        summary: "resolved".to_string(),
        evidence: Vec::new(),
        retry_key: None,
        question: None,
        requested_action: None,
    };
    first
        .apply_diagnosis(&token, response.clone(), true)
        .unwrap();
    assert!(matches!(
        first.apply_diagnosis(&token, response.clone(), true),
        Err(deepseek_custom::recovery::RecoveryStateError::DuplicateResult)
    ));

    let mut other_input = github_input("https://github.com/tychohenzen/deepseekcustom/pull/42");
    other_input.project = Some(ProjectReference::github("TychoHenzen/99"));
    let mut other = coordinator.start_run(&other_input, "verify", 1).unwrap();
    let other_token = other
        .begin_diagnosis(FailureContext::new("verify", "failure", &[]), Vec::new())
        .unwrap();
    assert!(matches!(
        first.apply_diagnosis(&other_token, response, true),
        Err(deepseek_custom::recovery::RecoveryStateError::StaleResult)
    ));
    assert_eq!(first.status(), RecoveryStatus::Resolved);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn retry_limit_and_interrupted_diagnostic_become_explicit_decisions() {
    let directory = temp_dir("limits");
    let interrupt = Arc::new(AtomicBool::new(false));
    let factory = Arc::new(
        BackendFactory::new(Settings::default(), PathBuf::from("."))
            .with_interrupt_flag(Arc::clone(&interrupt))
            .with_stub(
                "diagnostic",
                vec![StubTurn::Text(response_json(
                    "retryable",
                    r#""retry_key":"safe-reset","question":null,"requested_action":null"#,
                ))],
            ),
    );
    let coordinator = coordinator(factory, directory.clone());
    let mut run = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();
    coordinator
        .diagnose(&mut run, "first failure", &[], vec![retry_spec()])
        .await
        .unwrap();
    let action = CountingAction {
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    };
    coordinator.execute_retry(&mut run, &action).await.unwrap();

    coordinator
        .diagnose(&mut run, "second failure", &[], vec![retry_spec()])
        .await
        .unwrap();
    let limit = coordinator
        .execute_retry(&mut run, &action)
        .await
        .unwrap_err();
    assert!(limit.to_string().contains("retry limit"));
    assert_eq!(run.status(), RecoveryStatus::NeedsDecision);
    assert_eq!(action.calls.load(Ordering::SeqCst), 1);

    interrupt.store(true, Ordering::SeqCst);
    let mut interrupted = coordinator
        .start_run(
            &github_input("https://github.com/tychohenzen/deepseekcustom/pull/42"),
            "verify",
            1,
        )
        .unwrap();
    let status = coordinator
        .diagnose(&mut interrupted, "interrupted failure", &[], Vec::new())
        .await
        .unwrap();
    assert_eq!(status, RecoveryStatus::NeedsDecision);
    assert!(interrupted.record().question.is_some());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persisted_inflight_recovery_is_recovered_as_a_user_decision() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let mut run = RecoveryRun::new(identity, "verify", 2);
    run.begin_diagnosis(FailureContext::new("verify", "failure", &[]), Vec::new())
        .unwrap();

    let recovered = RecoveryRun::from_record(run.record().clone()).unwrap();

    assert_eq!(recovered.status(), RecoveryStatus::NeedsDecision);
    assert!(recovered.record().question.is_some());
}

#[test]
fn persisted_pending_retry_is_not_replayed_after_restart() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let mut run = RecoveryRun::new(identity, "verify", 2);
    let token = run
        .begin_diagnosis(
            FailureContext::new("verify", "failure", &[]),
            vec![retry_spec()],
        )
        .unwrap();
    run.apply_diagnosis(
        &token,
        DiagnosticResponse {
            outcome: DiagnosisOutcome::Retryable,
            summary: "retry once".to_string(),
            evidence: Vec::new(),
            retry_key: Some("safe-reset".to_string()),
            question: None,
            requested_action: None,
        },
        true,
    )
    .unwrap();
    run.claim_retry().unwrap();

    let recovered = RecoveryRun::from_record(run.record().clone()).unwrap();

    assert_eq!(recovered.status(), RecoveryStatus::NeedsDecision);
    assert!(recovered.record().pending_retry_key.is_none());
    assert!(
        recovered
            .record()
            .attempted_actions
            .iter()
            .any(|action| action.status == AttemptStatus::Pending)
    );
}

#[test]
fn retained_decision_fields_are_sanitized() {
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let mut run = RecoveryRun::new(identity, "verify", 1);
    let token = run
        .begin_diagnosis(FailureContext::new("verify", "failure", &[]), Vec::new())
        .unwrap();
    run.apply_diagnosis(
        &token,
        DiagnosticResponse {
            outcome: DiagnosisOutcome::NeedsDecision,
            summary: "requires a decision".to_string(),
            evidence: Vec::new(),
            retry_key: None,
            question: Some(r#"{"token":"plain-secret"} \\server\share\secret.txt"#.to_string()),
            requested_action: Some("token=another-secret".to_string()),
        },
        true,
    )
    .unwrap();

    assert!(
        !run.record()
            .question
            .as_deref()
            .unwrap()
            .contains("plain-secret")
    );
    assert!(
        !run.record()
            .next_required_decision
            .as_deref()
            .unwrap()
            .contains("another-secret")
    );
}

#[test]
fn sanitize_text_redacts_common_provider_tokens() {
    let text = sanitize_text(
        r#"ghp_secret github_pat_more sk-secret token=plain {"token":"plain-json"} \\server\share\secret.txt"#,
    );
    assert!(!text.contains("ghp_secret"));
    assert!(!text.contains("github_pat_more"));
    assert!(!text.contains("sk-secret"));
    assert!(!text.contains("plain"));
    assert!(!text.contains("plain-json"));
    assert!(!text.contains("\\\\server\\share\\secret.txt"));

    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let prompt = render_diagnostic_prompt(
        &identity,
        &FailureContext::new("verify", "failure", &[]),
        &[SafeRetrySpec {
            key: "safe-reset".to_string(),
            description: "token=retry-secret".to_string(),
        }],
    );
    assert!(!prompt.contains("retry-secret"));

    let direct_prompt = render_diagnostic_prompt(
        &identity,
        &FailureContext {
            current_step: "C:\\private\\step".to_string(),
            failure: "token=direct-secret".to_string(),
            prior_evidence: vec!["\\\\server\\share\\private.txt".to_string()],
        },
        &[],
    );
    assert!(!direct_prompt.contains("direct-secret"));
    assert!(!direct_prompt.contains("C:\\private\\step"));
    assert!(!direct_prompt.contains("\\\\server\\share\\private.txt"));
}

#[test]
fn recovery_record_round_trips_through_store() {
    let directory = temp_dir("store");
    let store = RecoveryStore::new(directory.clone());
    let identity = resolve_identity(&github_input(
        "https://github.com/tychohenzen/deepseekcustom/pull/42",
    ))
    .unwrap();
    let run = RecoveryRun::new(identity, "verify", 2);
    store.save(&run).unwrap();
    let loaded = store.load(&run.id()).unwrap();
    assert_eq!(loaded.record(), run.record());
    std::fs::remove_dir_all(directory).unwrap();
}
