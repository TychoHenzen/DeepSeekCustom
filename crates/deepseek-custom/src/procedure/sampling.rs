//! Deterministic representation and bounded dispatch of localization samples.

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
