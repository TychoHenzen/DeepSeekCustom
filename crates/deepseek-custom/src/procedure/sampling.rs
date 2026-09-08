//! Deterministic representation and bounded dispatch of localization samples.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use thiserror::Error;

use super::LocalizationTarget;
use super::{
    LocalizationDispatch, LocalizationDispatchError, LocalizationPromptInput, RepositoryIndexEntry,
    ValidatedSamplingInput, build_localization_prompt, validate_localization_targets,
};
use crate::config::settings::ValidatedProcedureSamplingSettings;

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One repository location used when comparing localization samples.
///
/// Evidence explains a model response but is not an identity. Agreement only
/// considers the repository path and optional symbol that survived target
/// validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedLocalizationTarget {
    pub path: String,
    pub symbol: Option<String>,
}

/// Ordered, deduplicated identities from one accepted localization response.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedLocalizationTargets(Vec<NormalizedLocalizationTarget>);

impl NormalizedLocalizationTargets {
    /// Discard non-identity evidence, then sort and deduplicate target identities.
    pub fn from_accepted(targets: &[LocalizationTarget]) -> Self {
        let mut normalized = targets
            .iter()
            .map(|target| NormalizedLocalizationTarget {
                path: target.path.clone(),
                symbol: target.symbol.clone(),
            })
            .collect::<Vec<_>>();
        normalized.sort_unstable();
        normalized.dedup();
        Self(normalized)
    }

    /// Identities in stable repository-path and symbol order.
    pub fn targets(&self) -> &[NormalizedLocalizationTarget] {
        &self.0
    }
}

/// Result of one bounded localization sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationSample {
    pub number: u8,
    pub outcome: LocalizationSampleOutcome,
}

/// The accepted or stopped outcome of one localization dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalizationSampleOutcome {
    Accepted {
        targets: Vec<LocalizationTarget>,
        normalized_targets: NormalizedLocalizationTargets,
    },
    Rejected {
        targets: Vec<LocalizationTarget>,
        error: String,
    },
    Interrupted,
}

/// All sample outcomes from one bounded local localization pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationSamplingRun {
    samples: Vec<LocalizationSample>,
}

/// One accepted normalized target set that met the configured agreement quorum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationAgreement {
    /// The first accepted response in the selected group, retained for its evidence.
    pub targets: Vec<LocalizationTarget>,
    /// The exact target identities shared by the selected group.
    pub normalized_targets: NormalizedLocalizationTargets,
    /// Sample numbers belonging to the selected group, in dispatch order.
    pub sample_numbers: Vec<u8>,
}

/// The bounded reason that a localization result left the local sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalizationEscalationTrigger {
    /// No exact normalized target group reached the configured quorum.
    LocalDisagreement,
}

impl std::fmt::Display for LocalizationEscalationTrigger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalDisagreement => formatter.write_str("local_disagreement"),
        }
    }
}

/// The selected source of a localization result after bounded sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalizationAgreementOutcome {
    /// An exact local group met quorum.
    Local { agreement: LocalizationAgreement },
    /// One frontier localization replaced disagreeing local samples.
    Frontier {
        targets: Vec<LocalizationTarget>,
        normalized_targets: NormalizedLocalizationTargets,
        escalation_trigger: LocalizationEscalationTrigger,
    },
    /// Shared interruption stopped sampling before a result could be selected.
    Interrupted,
}

/// The local sample evidence and selected localization source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationAgreementRun {
    pub sampling_run: LocalizationSamplingRun,
    pub outcome: LocalizationAgreementOutcome,
}

/// Failure while selecting a localization result after local sampling.
#[derive(Debug, Error)]
pub enum LocalizationAgreementError {
    #[error(transparent)]
    Sampling(#[from] LocalizationSamplingError),
    #[error("failed to serialize the frontier localization prompt after {trigger}: {reason}")]
    FrontierPromptSerialization {
        trigger: LocalizationEscalationTrigger,
        reason: String,
    },
    #[error("frontier localization dispatch after {trigger} failed: {source}")]
    FrontierDispatch {
        trigger: LocalizationEscalationTrigger,
        #[source]
        source: LocalizationDispatchError,
    },
    #[error("frontier localization targets after {trigger} failed validation: {source}")]
    FrontierValidation {
        trigger: LocalizationEscalationTrigger,
        #[source]
        source: super::LocalizationTargetValidationError,
    },
}

/// Select the largest exact normalized-target group that reaches `quorum`.
///
/// Equal-size qualifying groups choose the group whose first sample was
/// dispatched first. Rejected and interrupted samples do not contribute to a
/// group.
pub fn select_localization_agreement(
    samples: &[LocalizationSample],
    quorum: u8,
) -> Option<LocalizationAgreement> {
    let mut groups = BTreeMap::<NormalizedLocalizationTargets, AgreementGroup>::new();

    for sample in samples {
        let LocalizationSampleOutcome::Accepted {
            targets,
            normalized_targets,
        } = &sample.outcome
        else {
            continue;
        };

        let group = groups
            .entry(normalized_targets.clone())
            .or_insert_with(|| AgreementGroup {
                first_sample_number: sample.number,
                representative_targets: targets.clone(),
                sample_numbers: Vec::new(),
            });
        if sample.number < group.first_sample_number {
            group.first_sample_number = sample.number;
            group.representative_targets = targets.clone();
        }
        group.sample_numbers.push(sample.number);
    }

    groups
        .into_iter()
        .filter(|(_, group)| group.sample_numbers.len() >= usize::from(quorum))
        .min_by(|(_, left), (_, right)| {
            right
                .sample_numbers
                .len()
                .cmp(&left.sample_numbers.len())
                .then_with(|| left.first_sample_number.cmp(&right.first_sample_number))
        })
        .map(|(normalized_targets, mut group)| {
            group.sample_numbers.sort_unstable();
            LocalizationAgreement {
                targets: group.representative_targets,
                normalized_targets,
                sample_numbers: group.sample_numbers,
            }
        })
}

#[derive(Debug)]
struct AgreementGroup {
    first_sample_number: u8,
    representative_targets: Vec<LocalizationTarget>,
    sample_numbers: Vec<u8>,
}

/// Resolves local agreement first, then makes at most one frontier localization call.
pub struct LocalizationAgreementResolver<L, F> {
    local_sampler: LocalizationSampler<L>,
    frontier_dispatcher: F,
}

impl<L, F> LocalizationAgreementResolver<L, F>
where
    L: LocalizationDispatch,
    F: LocalizationDispatch,
{
    /// Construct a resolver from a validated local sampler and a frontier dispatcher.
    pub fn new(local_sampler: LocalizationSampler<L>, frontier_dispatcher: F) -> Self {
        Self {
            local_sampler,
            frontier_dispatcher,
        }
    }

    /// Select an exact local quorum or run one validated frontier localization.
    pub async fn resolve(
        &self,
        input: &ValidatedSamplingInput,
        repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationAgreementRun, LocalizationAgreementError> {
        let sampling_run = self.local_sampler.run(input, repository_index).await?;
        if sampling_run.interrupted() {
            return Ok(LocalizationAgreementRun {
                sampling_run,
                outcome: LocalizationAgreementOutcome::Interrupted,
            });
        }

        if let Some(agreement) = select_localization_agreement(
            sampling_run.samples(),
            self.local_sampler.agreement_quorum(),
        ) {
            return Ok(LocalizationAgreementRun {
                sampling_run,
                outcome: LocalizationAgreementOutcome::Local { agreement },
            });
        }

        let trigger = LocalizationEscalationTrigger::LocalDisagreement;
        let prompt = build_localization_prompt(LocalizationPromptInput {
            contract: &input.contract.contract,
            repository_index,
            scratchpad: &input.report.scratchpad,
        })
        .map_err(
            |error| LocalizationAgreementError::FrontierPromptSerialization {
                trigger,
                reason: error.to_string(),
            },
        )?;
        let envelope = self
            .frontier_dispatcher
            .dispatch_prompt(prompt, repository_index)
            .await
            .map_err(|source| LocalizationAgreementError::FrontierDispatch { trigger, source })?;
        let targets = validate_localization_targets(envelope.targets, repository_index)
            .map_err(|source| LocalizationAgreementError::FrontierValidation { trigger, source })?;
        let normalized_targets = NormalizedLocalizationTargets::from_accepted(&targets);

        Ok(LocalizationAgreementRun {
            sampling_run,
            outcome: LocalizationAgreementOutcome::Frontier {
                targets,
                normalized_targets,
                escalation_trigger: trigger,
            },
        })
    }
}

impl LocalizationSamplingRun {
    /// Outcomes sorted by configured sample number, not completion order.
    pub fn samples(&self) -> &[LocalizationSample] {
        &self.samples
    }

    /// Whether shared interruption stopped at least one sample.
    pub fn interrupted(&self) -> bool {
        self.samples
            .iter()
            .any(|sample| matches!(sample.outcome, LocalizationSampleOutcome::Interrupted))
    }
}

/// Failure before a bounded sampling dispatch can be constructed.
#[derive(Debug, Error)]
pub enum LocalizationSamplingError {
    #[error("failed to serialize the localization prompt: {0}")]
    PromptSerialization(String),
}

/// Runs the configured local localization samples after the input gate succeeds.
pub struct LocalizationSampler<D> {
    dispatcher: D,
    settings: ValidatedProcedureSamplingSettings,
    interrupt: Arc<AtomicBool>,
}

impl<D> LocalizationSampler<D>
where
    D: LocalizationDispatch,
{
    /// Construct a sampler from settings that already passed pre-dispatch validation.
    pub fn new(
        dispatcher: D,
        settings: ValidatedProcedureSamplingSettings,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            dispatcher,
            settings,
            interrupt,
        }
    }

    /// The quorum validated alongside this sampler's bounded sample count.
    pub const fn agreement_quorum(&self) -> u8 {
        self.settings.localization_agreement_quorum()
    }

    /// Attempt each configured local sample unless shared interruption stops it.
    ///
    /// `input` is the successful result of [`super::SamplingInputGate::load`].
    /// With the setting cap at five, this keeps no more than five futures in
    /// flight while still allowing the localizer calls to overlap.
    pub async fn run(
        &self,
        input: &ValidatedSamplingInput,
        repository_index: &[RepositoryIndexEntry],
    ) -> Result<LocalizationSamplingRun, LocalizationSamplingError> {
        let prompt = build_localization_prompt(LocalizationPromptInput {
            contract: &input.contract.contract,
            repository_index,
            scratchpad: &input.report.scratchpad,
        })
        .map_err(|error| LocalizationSamplingError::PromptSerialization(error.to_string()))?;

        let mut pending = FuturesUnordered::new();
        for number in 1..=self.settings.localization_sample_count() {
            if self.interrupted() {
                break;
            }
            pending.push(self.dispatch_sample(number, prompt.clone(), repository_index));
        }

        let mut samples = Vec::new();
        while let Some(sample) = pending.next().await {
            samples.push(sample);
        }
        samples.sort_unstable_by_key(|sample| sample.number);
        Ok(LocalizationSamplingRun { samples })
    }

    async fn dispatch_sample(
        &self,
        number: u8,
        prompt: String,
        repository_index: &[RepositoryIndexEntry],
    ) -> LocalizationSample {
        if self.interrupted() {
            return interrupted_sample(number);
        }

        let dispatch = self.dispatcher.dispatch_prompt(prompt, repository_index);
        tokio::pin!(dispatch);
        let response = loop {
            tokio::select! {
                result = &mut dispatch => break Some(result),
                _ = tokio::time::sleep(INTERRUPT_POLL_INTERVAL) => {
                    if self.interrupted() {
                        break None;
                    }
                }
            }
        };

        let Some(response) = response else {
            return interrupted_sample(number);
        };
        if self.interrupted() {
            return interrupted_sample(number);
        }
        match response {
            Ok(envelope) => {
                match validate_localization_targets(envelope.targets.clone(), repository_index) {
                    Ok(targets) => LocalizationSample {
                        number,
                        outcome: LocalizationSampleOutcome::Accepted {
                            normalized_targets: NormalizedLocalizationTargets::from_accepted(
                                &targets,
                            ),
                            targets,
                        },
                    },
                    Err(error) => LocalizationSample {
                        number,
                        outcome: LocalizationSampleOutcome::Rejected {
                            targets: envelope.targets,
                            error: error.to_string(),
                        },
                    },
                }
            }
            Err(error) => LocalizationSample {
                number,
                outcome: LocalizationSampleOutcome::Rejected {
                    targets: Vec::new(),
                    error: dispatch_error_message(error),
                },
            },
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }
}

fn interrupted_sample(number: u8) -> LocalizationSample {
    LocalizationSample {
        number,
        outcome: LocalizationSampleOutcome::Interrupted,
    }
}

fn dispatch_error_message(error: LocalizationDispatchError) -> String {
    error.to_string()
}
