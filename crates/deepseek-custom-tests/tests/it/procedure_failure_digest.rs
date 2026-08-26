use std::time::Duration;

use deepseek_custom::procedure::{
    BoundedVerifierOutput, FAILURE_COMMAND_CHARACTER_CAP, FAILURE_DIAGNOSTIC_CHARACTER_CAP,
    FailureDigest, FailureDigestErrorCategory, RepairTier, VerifierCommandDisposition,
    VerifierCommandResult, build_failure_digest_section,
};

fn failed_result(command: &str, exit_code: Option<i32>, diagnostic: &str) -> VerifierCommandResult {
    VerifierCommandResult {
        command: command.to_string(),
        disposition: VerifierCommandDisposition::Failed,
        success: false,
        exit_code,
        stdout: BoundedVerifierOutput {
            text: String::new(),
            first_edge: String::new(),
            last_edge: String::new(),
            truncated: false,
            bytes_seen: 0,
        },
        stderr: BoundedVerifierOutput {
            text: diagnostic.to_string(),
            first_edge: diagnostic.to_string(),
            last_edge: diagnostic.to_string(),
            truncated: false,
            bytes_seen: diagnostic.len() as u64,
        },
        combined_output: BoundedVerifierOutput {
            text: diagnostic.to_string(),
            first_edge: diagnostic.to_string(),
            last_edge: diagnostic.to_string(),
            truncated: false,
            bytes_seen: diagnostic.len() as u64,
        },
        duration: Duration::from_millis(12),
        error: None,
    }
}

#[test]
fn digest_bounds_command_text_and_rejects_caps_that_cannot_identify_the_newest_failure() {
    let result = failed_result(&format!("verify {}", "x".repeat(800)), Some(1), "failure");
    let digest = FailureDigest::from_verifier_result(1, RepairTier::Local, &result);

    assert_eq!(
        digest.command.chars().count(),
        FAILURE_COMMAND_CHARACTER_CAP
    );
    assert!(digest.command.ends_with("...[truncated]"));
    assert_eq!(
        build_failure_digest_section(&[digest], 127)
            .unwrap_err()
            .to_string(),
        "failure-section character cap must be at least 128, got 127"
    );
}

#[test]
fn digest_keeps_exact_typed_failure_fields_and_bounds_diagnostic() {
    let result = failed_result("cargo test --workspace", Some(17), &"x".repeat(5_000));

    let digest = FailureDigest::from_verifier_result(3, RepairTier::Local, &result);

    assert_eq!(digest.attempt_number, 3);
    assert_eq!(digest.tier, RepairTier::Local);
    assert_eq!(digest.command, "cargo test --workspace");
    assert_eq!(digest.exit_code, Some(17));
    assert_eq!(
        digest.error_category,
        FailureDigestErrorCategory::VerifierFailed
    );
    assert_eq!(
        digest.diagnostic.chars().count(),
        FAILURE_DIAGNOSTIC_CHARACTER_CAP
    );
    assert!(digest.diagnostic.ends_with("...[truncated]"));
    assert_eq!(
        serde_json::from_str::<FailureDigest>(&serde_json::to_string(&digest).unwrap()).unwrap(),
        digest
    );
}

#[test]
fn section_orders_attempts_and_keeps_only_the_newest_detailed_output() {
    let third = FailureDigest::from_verifier_result(
        3,
        RepairTier::Local,
        &failed_result("verify-three", Some(3), "DETAIL_THREE"),
    );
    let first = FailureDigest::from_verifier_result(
        1,
        RepairTier::Local,
        &failed_result("verify-one", Some(1), "DETAIL_ONE"),
    );
    let second = FailureDigest::from_verifier_result(
        2,
        RepairTier::Local,
        &failed_result("verify-two", Some(2), "DETAIL_TWO"),
    );

    let section = build_failure_digest_section(&[third, first, second], 4_000).unwrap();

    assert!(section.find("attempt=1").unwrap() < section.find("attempt=2").unwrap());
    assert!(section.find("attempt=2").unwrap() < section.find("attempt=3").unwrap());
    assert!(!section.contains("DETAIL_ONE"));
    assert!(!section.contains("DETAIL_TWO"));
    assert!(section.contains("DETAIL_THREE"));
    assert_eq!(section.matches("diagnostic:").count(), 1);
}

#[test]
fn total_cap_drops_old_summaries_before_truncating_newest_failure() {
    let failures = (1..=3)
        .map(|attempt| {
            FailureDigest::from_verifier_result(
                attempt,
                RepairTier::Local,
                &failed_result(
                    &format!("verify-{attempt}"),
                    Some(attempt.into()),
                    &format!("newest-diagnostic-{attempt}-{}", "z".repeat(120)),
                ),
            )
        })
        .collect::<Vec<_>>();

    let section = build_failure_digest_section(&failures, 160).unwrap();

    assert!(section.chars().count() <= 160);
    assert!(section.contains("attempt=3"));
    assert!(section.contains("newest-diagnostic-3"));
    assert!(!section.contains("attempt=1"));
}

#[test]
fn digest_excludes_secret_source_prompt_and_chat_material() {
    let diagnostic = concat!(
        "assertion failed at verifier gate\n",
        "api_key=TOP_SECRET\n",
        "Authorization Bearer BEARER_SECRET\n",
        "source_contents=SOURCE_SENTINEL\n",
        "prior_prompt=PRIOR_PROMPT_SENTINEL\n",
        "chat_history=CHAT_SENTINEL\n",
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "+ SOURCE_LINE_SENTINEL\n",
    );
    let result = failed_result(
        "verify --token COMMAND_SECRET --api_key=INLINE_SECRET",
        Some(1),
        diagnostic,
    );

    let digest = FailureDigest::from_verifier_result(1, RepairTier::Local, &result);
    let serialized = serde_json::to_string(&digest).unwrap();

    for forbidden in [
        "TOP_SECRET",
        "BEARER_SECRET",
        "SOURCE_SENTINEL",
        "PRIOR_PROMPT_SENTINEL",
        "CHAT_SENTINEL",
        "SOURCE_LINE_SENTINEL",
        "COMMAND_SECRET",
        "INLINE_SECRET",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "leaked {forbidden}: {serialized}"
        );
    }
    assert!(
        digest
            .diagnostic
            .contains("assertion failed at verifier gate")
    );
    assert!(digest.command.contains("--token [redacted]"));
    assert!(digest.command.contains("--api_key=[redacted]"));
}
