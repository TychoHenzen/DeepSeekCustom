use crate::procedure::{VerifierGateDisposition, VerifierGateEvidence};

use super::{ControlledDevelopmentPhase, WorkCard};

pub const MAX_PROGRESS_NOTICE_WORDS: usize = 80;
pub const MAX_COMPLETION_SUMMARY_WORDS: usize = 200;

/// Count words using the same deterministic rule used by both compact builders.
pub fn whitespace_word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

/// Build a compact, harness-owned notice for every lifecycle phase.
pub fn build_progress_notice(
    phase: ControlledDevelopmentPhase,
    card: Option<&WorkCard>,
    changed_paths: &[String],
    proof_evidence: &[VerifierGateEvidence],
    has_blocker: bool,
) -> String {
    let card_state = if card.is_some() {
        "A Work Card is recorded."
    } else {
        "No Work Card is recorded."
    };
    let blocker_state = if has_blocker {
        "A failure is recorded in the blocker field."
    } else {
        "No failure is recorded."
    };
    let notice = format!(
        "Phase: {}. {card_state} {} changed path(s) and {} proof result(s) are recorded. {blocker_state}",
        phase_label(phase),
        changed_paths.len(),
        proof_evidence.len(),
    );
    debug_assert!(whitespace_word_count(&notice) <= MAX_PROGRESS_NOTICE_WORDS);
    notice
}

/// Build a terminal summary from typed workflow fields without accepting backend text.
pub fn build_completion_summary(
    phase: ControlledDevelopmentPhase,
    card: Option<&WorkCard>,
    changed_paths: &[String],
    proof_evidence: &[VerifierGateEvidence],
    has_blocker: bool,
    limitation: &str,
) -> Option<String> {
    if !matches!(
        phase,
        ControlledDevelopmentPhase::Completed
            | ControlledDevelopmentPhase::Blocked
            | ControlledDevelopmentPhase::Interrupted
    ) {
        return None;
    }

    let (passed, failed, interrupted, not_run) = proof_counts(proof_evidence);
    let proof_segment = format!(
        "Proof results: {passed} passed, {failed} failed, {interrupted} interrupted, {not_run} not run."
    );
    let failure_segment = has_blocker.then(|| {
        "Complete failure evidence is shown in the blocker and raw-details fields.".to_string()
    });
    let limitation_fallback = "Remaining limitation is shown in full below.".to_string();
    let card_fallback = if card.is_some() {
        "Work Card outcome is shown in full above.".to_string()
    } else {
        "No Work Card is recorded.".to_string()
    };
    let paths_fallback = if changed_paths.is_empty() {
        "Changed paths: none.".to_string()
    } else {
        format!(
            "{} exact changed path(s) are shown in the changed-paths field.",
            changed_paths.len()
        )
    };
    let mut segments = vec![
        format!("Phase: {}.", phase_label(phase)),
        card_fallback,
        paths_fallback,
        proof_segment,
    ];
    if let Some(failure_segment) = failure_segment {
        segments.push(failure_segment);
    }
    segments.push(limitation_fallback);

    if let Some(card) = card {
        replace_if_fits(
            &mut segments,
            1,
            format!("Work Card outcome: {}.", card.outcome),
        );
    }
    if !changed_paths.is_empty() {
        replace_if_fits(
            &mut segments,
            2,
            format!("Changed paths: {}.", changed_paths.join(", ")),
        );
    }
    let limitation_index = segments.len() - 1;
    replace_if_fits(
        &mut segments,
        limitation_index,
        format!("Remaining limitation: {limitation}"),
    );

    let summary = segments.join(" ");
    debug_assert!(whitespace_word_count(&summary) <= MAX_COMPLETION_SUMMARY_WORDS);
    Some(summary)
}

fn replace_if_fits(segments: &mut [String], index: usize, replacement: String) {
    let current_words = segments
        .iter()
        .map(|segment| whitespace_word_count(segment))
        .sum::<usize>();
    let candidate_words = current_words - whitespace_word_count(&segments[index])
        + whitespace_word_count(&replacement);
    if candidate_words <= MAX_COMPLETION_SUMMARY_WORDS {
        segments[index] = replacement;
    }
}

fn proof_counts(evidence: &[VerifierGateEvidence]) -> (usize, usize, usize, usize) {
    evidence.iter().fold(
        (0, 0, 0, 0),
        |(passed, failed, interrupted, not_run), proof| match proof.disposition {
            VerifierGateDisposition::Passed => (passed + 1, failed, interrupted, not_run),
            VerifierGateDisposition::Failed | VerifierGateDisposition::SpawnFailed => {
                (passed, failed + 1, interrupted, not_run)
            }
            VerifierGateDisposition::Interrupted => (passed, failed, interrupted + 1, not_run),
            VerifierGateDisposition::NotRun { .. }
            | VerifierGateDisposition::NotRunAfterPatch { .. } => {
                (passed, failed, interrupted, not_run + 1)
            }
        },
    )
}

fn phase_label(phase: ControlledDevelopmentPhase) -> &'static str {
    match phase {
        ControlledDevelopmentPhase::Off => "Off",
        ControlledDevelopmentPhase::Planning => "Planning",
        ControlledDevelopmentPhase::AwaitingApproval => "Awaiting approval",
        ControlledDevelopmentPhase::Executing => "Executing",
        ControlledDevelopmentPhase::Completed => "Completed",
        ControlledDevelopmentPhase::Blocked => "Blocked",
        ControlledDevelopmentPhase::Interrupted => "Interrupted",
    }
}
