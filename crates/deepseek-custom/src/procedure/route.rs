//! Deterministic route evidence for patch preview drafting.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The model class selected to draft one patch preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteTier {
    Local,
    Frontier,
}

/// A route choice that applies to one patch-preview request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteOverride {
    #[default]
    Automatic,
    ForceLocal,
    ForceFrontier,
}

impl fmt::Display for RouteOverride {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Automatic => "automatic",
            Self::ForceLocal => "force local",
            Self::ForceFrontier => "force frontier",
        })
    }
}

impl fmt::Display for RouteTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local => "local",
            Self::Frontier => "frontier",
        })
    }
}

/// An explicit verb that describes mechanical work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MechanicalVerb {
    Rename,
    Import,
    SignaturePropagation,
    Boilerplate,
    TestScaffolding,
    Formatting,
    Documentation,
}

impl fmt::Display for MechanicalVerb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Rename => "rename",
            Self::Import => "import",
            Self::SignaturePropagation => "signature propagation",
            Self::Boilerplate => "boilerplate",
            Self::TestScaffolding => "test scaffolding",
            Self::Formatting => "formatting",
            Self::Documentation => "documentation",
        })
    }
}

/// One deterministic signal used to explain a patch-preview route.
///
/// The declaration order is the stable evidence order used by route reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RouteSignal {
    MechanicalVerb(MechanicalVerb),
    TargetCount(usize),
    Architecture,
    CrossCuttingBehavior,
    Concurrency,
    Security,
    Migration,
    PublicApi,
    SubtleBug,
    SubstantiveLogic,
    UnknownWording,
}

impl fmt::Display for RouteSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MechanicalVerb(verb) => write!(formatter, "mechanical verb: {verb}"),
            Self::TargetCount(count) => write!(formatter, "target count: {count}"),
            Self::Architecture => formatter.write_str("architecture"),
            Self::CrossCuttingBehavior => formatter.write_str("cross-cutting behavior"),
            Self::Concurrency => formatter.write_str("concurrency"),
            Self::Security => formatter.write_str("security"),
            Self::Migration => formatter.write_str("migration"),
            Self::PublicApi => formatter.write_str("public API"),
            Self::SubtleBug => formatter.write_str("subtle bug"),
            Self::SubstantiveLogic => formatter.write_str("substantive logic"),
            Self::UnknownWording => formatter.write_str("unknown wording"),
        }
    }
}

/// The automatic tier and all deterministic evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DifficultyAssessment {
    pub tier: RouteTier,
    pub signals: Vec<RouteSignal>,
}

/// The automatic route evidence and the effective tier for one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub automatic_tier: RouteTier,
    pub effective_tier: RouteTier,
    pub signals: Vec<RouteSignal>,
    pub selected_override: RouteOverride,
    pub overridden: bool,
}

/// Apply one request-scoped override without discarding the automatic route.
pub fn apply_route_override(
    assessment: DifficultyAssessment,
    selected_override: RouteOverride,
) -> RouteDecision {
    let effective_tier = match selected_override {
        RouteOverride::Automatic => assessment.tier,
        RouteOverride::ForceLocal => RouteTier::Local,
        RouteOverride::ForceFrontier => RouteTier::Frontier,
    };
    RouteDecision {
        automatic_tier: assessment.tier,
        effective_tier,
        signals: assessment.signals,
        selected_override,
        overridden: effective_tier != assessment.tier,
    }
}

/// Assess a route using only the selected contract text and localized target count.
pub fn assess_route(contract_text: &str, localized_target_count: usize) -> DifficultyAssessment {
    let words = normalized_words(contract_text);
    let mut signals = mechanical_signals(&words);
    signals.push(RouteSignal::TargetCount(localized_target_count));
    signals.extend(frontier_signals(&words));

    let has_mechanical_verb = signals
        .iter()
        .any(|signal| matches!(signal, RouteSignal::MechanicalVerb(_)));
    let has_frontier_marker = signals.iter().any(is_frontier_marker);
    if !has_mechanical_verb && !has_frontier_marker {
        signals.push(RouteSignal::UnknownWording);
    }
    signals.sort();
    signals.dedup();

    let tier = if localized_target_count == 1 && has_mechanical_verb && !has_frontier_marker {
        RouteTier::Local
    } else {
        RouteTier::Frontier
    };
    DifficultyAssessment { tier, signals }
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn mechanical_signals(words: &[String]) -> Vec<RouteSignal> {
    let candidates = [
        (
            MechanicalVerb::Rename,
            &["rename", "renames", "renaming"][..],
        ),
        (MechanicalVerb::Import, &["import", "imports", "importing"]),
        (
            MechanicalVerb::SignaturePropagation,
            &["signature propagation", "propagate signature"],
        ),
        (MechanicalVerb::Boilerplate, &["boilerplate"]),
        (
            MechanicalVerb::TestScaffolding,
            &["test scaffolding", "test scaffold", "scaffold tests"],
        ),
        (MechanicalVerb::Formatting, &["format", "formatting"]),
        (
            MechanicalVerb::Documentation,
            &["documentation", "document", "docs"],
        ),
    ];
    candidates
        .into_iter()
        .filter_map(|(verb, phrases)| {
            phrases
                .iter()
                .any(|phrase| contains_phrase(words, phrase))
                .then_some(RouteSignal::MechanicalVerb(verb))
        })
        .collect()
}

fn frontier_signals(words: &[String]) -> Vec<RouteSignal> {
    let candidates = [
        (
            RouteSignal::Architecture,
            &["architecture", "architectural"][..],
        ),
        (
            RouteSignal::CrossCuttingBehavior,
            &["cross cutting", "crosscutting"],
        ),
        (RouteSignal::Concurrency, &["concurrency", "concurrent"]),
        (
            RouteSignal::Security,
            &["security", "secure", "vulnerability", "vulnerabilities"],
        ),
        (
            RouteSignal::Migration,
            &[
                "migration",
                "migrations",
                "migrate",
                "migrates",
                "migrated",
                "migrating",
            ],
        ),
        (RouteSignal::PublicApi, &["public api"]),
        (RouteSignal::SubtleBug, &["subtle bug", "subtle bugs"]),
        (RouteSignal::SubstantiveLogic, &["substantive logic"]),
    ];
    candidates
        .into_iter()
        .filter_map(|(signal, phrases)| {
            phrases
                .iter()
                .any(|phrase| contains_phrase(words, phrase))
                .then_some(signal)
        })
        .collect()
}

fn contains_phrase(words: &[String], phrase: &str) -> bool {
    let phrase = phrase.split_whitespace().collect::<Vec<_>>();
    words
        .windows(phrase.len())
        .any(|window| window.iter().map(String::as_str).eq(phrase.iter().copied()))
}

fn is_frontier_marker(signal: &RouteSignal) -> bool {
    matches!(
        signal,
        RouteSignal::Architecture
            | RouteSignal::CrossCuttingBehavior
            | RouteSignal::Concurrency
            | RouteSignal::Security
            | RouteSignal::Migration
            | RouteSignal::PublicApi
            | RouteSignal::SubtleBug
            | RouteSignal::SubstantiveLogic
    )
}
