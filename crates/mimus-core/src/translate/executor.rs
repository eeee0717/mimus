use std::time::Duration;

use super::cache::{CachedTranslation, TranslationCache, TranslationCacheKey};
use super::{
    Glossary, PlaceholderViolation, PreparedTranslation, RedactedTranslationProfile,
    RestoredTranslation, Sleeper, TranslationRequest, Translator,
};
use crate::error::{MimusError, Result, RetryReason, TranslationReason};
use crate::event::CacheStatus;

const MAX_RETRIES: usize = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const MAX_CONSERVATION_TOKEN_SAMPLE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryAttempt {
    pub attempt: usize,
    pub delay_ms: u64,
    pub reason: RetryReason,
}

#[derive(Debug)]
pub(crate) enum TranslationOutcome {
    Translated {
        restored: RestoredTranslation,
        conservation: crate::il::TranslationConservationEvidence,
    },
    Identity {
        suspicious: bool,
        conservation: crate::il::TranslationConservationEvidence,
    },
    PlaceholderViolation {
        violation: PlaceholderViolation,
        profile: RedactedTranslationProfile,
    },
    ContentConservationViolation {
        missing_token_count: usize,
        missing_tokens: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaceholderRetryAttempt {
    pub attempt: usize,
    pub violation: PlaceholderViolation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentConservationRetryAttempt {
    pub attempt: usize,
    pub missing_token_count: usize,
    pub missing_tokens: Vec<String>,
}

pub(crate) struct TranslationExecution {
    pub cache_status: Option<CacheStatus>,
    pub retries: Vec<RetryAttempt>,
    pub placeholder_retries: Vec<PlaceholderRetryAttempt>,
    pub content_conservation_retries: Vec<ContentConservationRetryAttempt>,
    pub outcome: Result<TranslationOutcome>,
}

pub(crate) struct ExecutionRequest<'a> {
    pub prepared: &'a PreparedTranslation,
    pub target_language: &'a str,
    pub glossary: &'a Glossary,
    pub cache_key: &'a TranslationCacheKey,
}

pub(crate) fn execute(
    translator: &dyn Translator,
    cache: Option<&TranslationCache>,
    sleeper: &dyn Sleeper,
    request: ExecutionRequest<'_>,
) -> TranslationExecution {
    let cache_status = if let Some(cache) = cache {
        match cache.get(request.cache_key) {
            Ok(Some(CachedTranslation::Identity)) => {
                return TranslationExecution {
                    cache_status: Some(CacheStatus::Hit),
                    retries: Vec::new(),
                    placeholder_retries: Vec::new(),
                    content_conservation_retries: Vec::new(),
                    outcome: Ok(TranslationOutcome::Identity {
                        suspicious: request.prepared.echo_retry_eligible(),
                        conservation: request.prepared.identity_conservation_evidence(),
                    }),
                };
            }
            Ok(Some(CachedTranslation::Translated(validated))) => {
                if let Ok(restored) = request.prepared.restore(&validated)
                    && request
                        .prepared
                        .missing_conserved_tokens(&restored)
                        .is_empty()
                {
                    let conservation = request
                        .prepared
                        .conservation_evidence(&validated, &restored);
                    return TranslationExecution {
                        cache_status: Some(CacheStatus::Hit),
                        retries: Vec::new(),
                        placeholder_retries: Vec::new(),
                        content_conservation_retries: Vec::new(),
                        outcome: Ok(TranslationOutcome::Translated {
                            restored,
                            conservation,
                        }),
                    };
                }
                Some(CacheStatus::Miss)
            }
            Ok(None) => Some(CacheStatus::Miss),
            Err(error) => {
                return TranslationExecution {
                    cache_status: None,
                    retries: Vec::new(),
                    placeholder_retries: Vec::new(),
                    content_conservation_retries: Vec::new(),
                    outcome: Err(error),
                };
            }
        }
    } else {
        None
    };

    let mut retries = Vec::new();
    let mut placeholder_retries = Vec::new();
    let mut content_conservation_retries = Vec::new();
    let mut echo_retried = false;
    let mut placeholder_retried = false;
    let mut content_conservation_retried = false;
    let mut placeholder_correction = None;
    let mut content_correction = None;
    let mut response_attempt = 0;
    let outcome = loop {
        let provider_request = TranslationRequest {
            text: request.prepared.request_text(),
            target_language: request.target_language,
            glossary: request.glossary,
            placeholder_correction: placeholder_correction.as_deref(),
            content_correction: content_correction.as_deref(),
        };
        let (output, attempt_retries) =
            translate_with_retry(translator, sleeper, &provider_request);
        retries.extend(attempt_retries);
        let output = match output {
            Ok(output) => output,
            Err(error) => break Err(error),
        };
        response_attempt += 1;
        match classify_and_restore(request.prepared, &output) {
            ClassifiedOutcome::Identity
                if request.prepared.echo_retry_eligible() && !echo_retried =>
            {
                echo_retried = true;
            }
            ClassifiedOutcome::Identity => {
                let result = cache
                    .map(|cache| cache.insert_identity(request.cache_key))
                    .transpose()
                    .map(|_| TranslationOutcome::Identity {
                        suspicious: request.prepared.echo_retry_eligible(),
                        conservation: request.prepared.identity_conservation_evidence(),
                    });
                break result;
            }
            ClassifiedOutcome::Translated {
                validated,
                restored,
            } => {
                let missing = request.prepared.missing_conserved_tokens(&restored);
                if !missing.is_empty() && !content_conservation_retried {
                    content_conservation_retried = true;
                    let missing_token_count = missing.len();
                    let missing_tokens = bounded_conservation_tokens(&missing);
                    content_correction = Some(content_conservation_correction(&missing_tokens));
                    content_conservation_retries.push(ContentConservationRetryAttempt {
                        attempt: response_attempt,
                        missing_token_count,
                        missing_tokens,
                    });
                    continue;
                }
                if !missing.is_empty() {
                    break Ok(TranslationOutcome::ContentConservationViolation {
                        missing_token_count: missing.len(),
                        missing_tokens: bounded_conservation_tokens(&missing),
                    });
                }
                let conservation = request
                    .prepared
                    .conservation_evidence(&validated, &restored);
                let result = cache
                    .map(|cache| cache.insert(request.cache_key, &validated))
                    .transpose()
                    .map(|_| TranslationOutcome::Translated {
                        restored,
                        conservation,
                    });
                break result;
            }
            ClassifiedOutcome::PlaceholderViolation(violation) if !placeholder_retried => {
                placeholder_retried = true;
                placeholder_correction = Some(
                    request
                        .prepared
                        .placeholder_retry_correction(violation, &output),
                );
                placeholder_retries.push(PlaceholderRetryAttempt {
                    attempt: response_attempt,
                    violation,
                });
            }
            ClassifiedOutcome::PlaceholderViolation(violation) => {
                break Ok(TranslationOutcome::PlaceholderViolation {
                    violation,
                    profile: super::redacted_translation_profile(&output),
                });
            }
        }
    };
    TranslationExecution {
        cache_status,
        retries,
        placeholder_retries,
        content_conservation_retries,
        outcome,
    }
}

fn bounded_conservation_tokens(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .take(MAX_CONSERVATION_TOKEN_SAMPLE)
        .cloned()
        .collect()
}

fn content_conservation_correction(missing_tokens: &[String]) -> String {
    format!(
        "the previous response omitted conserved source tokens: {}. preserve every listed token or a lexically explicit equivalent.",
        missing_tokens.join(", ")
    )
}

enum ClassifiedOutcome {
    Translated {
        validated: super::ValidatedTranslation,
        restored: RestoredTranslation,
    },
    Identity,
    PlaceholderViolation(PlaceholderViolation),
}

fn classify_and_restore(prepared: &PreparedTranslation, output: &str) -> ClassifiedOutcome {
    match prepared.classify(output) {
        super::TranslationOutcome::Identity => ClassifiedOutcome::Identity,
        super::TranslationOutcome::Translated(validated) => match prepared.restore(&validated) {
            Ok(restored) => ClassifiedOutcome::Translated {
                validated,
                restored,
            },
            Err(violation) => ClassifiedOutcome::PlaceholderViolation(violation),
        },
        super::TranslationOutcome::PlaceholderViolation(violation) => {
            ClassifiedOutcome::PlaceholderViolation(violation)
        }
    }
}

fn translate_with_retry(
    translator: &dyn Translator,
    sleeper: &dyn Sleeper,
    request: &TranslationRequest<'_>,
) -> (Result<String>, Vec<RetryAttempt>) {
    let mut retries = Vec::new();
    for attempt in 1..=(MAX_RETRIES + 1) {
        match translator.translate(request) {
            Ok(output) => return (Ok(output), retries),
            Err(error) => {
                let Some(reason) = error.retry_reason() else {
                    return (Err(error), retries);
                };
                if attempt > MAX_RETRIES {
                    return (
                        Err(MimusError::translation(
                            TranslationReason::RetryExhausted,
                            format!("translation failed after {} attempts", MAX_RETRIES + 1),
                        )),
                        retries,
                    );
                }
                let delay = INITIAL_BACKOFF * (1_u32 << (attempt - 1));
                retries.push(RetryAttempt {
                    attempt,
                    delay_ms: delay.as_millis() as u64,
                    reason,
                });
                sleeper.sleep(delay);
            }
        }
    }
    unreachable!("the retry loop always returns")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::translate::{PreparedPart, ThreadSleeper};

    #[derive(Debug, Default)]
    struct RecordingSleeper {
        durations: Mutex<Vec<Duration>>,
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.lock().unwrap().push(duration);
        }
    }

    struct FlakyTranslator {
        failures: usize,
        retry_reason: Option<RetryReason>,
        calls: AtomicUsize,
    }

    struct ScriptedTranslator {
        outputs: Mutex<VecDeque<&'static str>>,
        placeholder_corrections: Mutex<Vec<Option<String>>>,
        content_corrections: Mutex<Vec<Option<String>>>,
        calls: AtomicUsize,
    }

    impl ScriptedTranslator {
        fn new(outputs: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().collect()),
                placeholder_corrections: Mutex::new(Vec::new()),
                content_corrections: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }

        fn placeholder_corrections(&self) -> Vec<Option<String>> {
            self.placeholder_corrections.lock().unwrap().clone()
        }

        fn content_corrections(&self) -> Vec<Option<String>> {
            self.content_corrections.lock().unwrap().clone()
        }
    }

    impl Translator for ScriptedTranslator {
        fn translate(&self, request: &TranslationRequest<'_>) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.placeholder_corrections
                .lock()
                .unwrap()
                .push(request.placeholder_correction.map(str::to_owned));
            self.content_corrections
                .lock()
                .unwrap()
                .push(request.content_correction.map(str::to_owned));
            Ok(self
                .outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted translation response")
                .to_owned())
        }
    }

    impl Translator for FlakyTranslator {
        fn translate(&self, _request: &TranslationRequest<'_>) -> Result<String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call <= self.failures {
                return Err(self.retry_reason.map_or_else(
                    || {
                        MimusError::translation(
                            TranslationReason::BackendRejected,
                            "permanent rejection",
                        )
                    },
                    |reason| {
                        MimusError::retryable_translation(
                            TranslationReason::TranslationFailed,
                            reason,
                            "transient failure",
                        )
                    },
                ));
            }
            Ok("translated".to_owned())
        }
    }

    fn prepared() -> PreparedTranslation {
        PreparedTranslation::new([PreparedPart::Text {
            text: "source".to_owned(),
            bold: false,
        }])
    }

    fn execute_without_cache(
        translator: &dyn Translator,
        sleeper: &dyn Sleeper,
    ) -> TranslationExecution {
        let prepared = prepared();
        execute_prepared_without_cache(&prepared, translator, sleeper)
    }

    fn execute_prepared_without_cache(
        prepared: &PreparedTranslation,
        translator: &dyn Translator,
        sleeper: &dyn Sleeper,
    ) -> TranslationExecution {
        let glossary = Glossary::default();
        execute(
            translator,
            None,
            sleeper,
            ExecutionRequest {
                prepared,
                target_language: "zh-CN",
                glossary: &glossary,
                cache_key: &TranslationCacheKey::new(
                    prepared.request_text(),
                    "model",
                    "zh-CN",
                    "prompt",
                    "glossary",
                ),
            },
        )
    }

    #[test]
    fn every_transient_class_uses_three_bounded_exponential_retries() {
        for reason in [
            RetryReason::RateLimited,
            RetryReason::Timeout,
            RetryReason::ServerError,
        ] {
            let translator = FlakyTranslator {
                failures: 3,
                retry_reason: Some(reason),
                calls: AtomicUsize::new(0),
            };
            let sleeper = RecordingSleeper::default();

            let execution = execute_without_cache(&translator, &sleeper);

            assert!(matches!(
                execution.outcome,
                Ok(TranslationOutcome::Translated { .. })
            ));
            assert_eq!(translator.calls.load(Ordering::SeqCst), 4);
            assert_eq!(
                execution.retries,
                [
                    RetryAttempt {
                        attempt: 1,
                        delay_ms: 250,
                        reason,
                    },
                    RetryAttempt {
                        attempt: 2,
                        delay_ms: 500,
                        reason,
                    },
                    RetryAttempt {
                        attempt: 3,
                        delay_ms: 1_000,
                        reason,
                    },
                ]
            );
            assert_eq!(
                *sleeper.durations.lock().unwrap(),
                [
                    Duration::from_millis(250),
                    Duration::from_millis(500),
                    Duration::from_millis(1_000),
                ]
            );
        }
    }

    #[test]
    fn permanent_failures_are_not_retried() {
        let translator = FlakyTranslator {
            failures: usize::MAX,
            retry_reason: None,
            calls: AtomicUsize::new(0),
        };
        let sleeper = RecordingSleeper::default();

        let execution = execute_without_cache(&translator, &sleeper);

        assert_eq!(
            execution.outcome.unwrap_err().reason(),
            crate::error::ErrorReason::Translation(TranslationReason::BackendRejected)
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
        assert!(execution.retries.is_empty());
        assert!(sleeper.durations.lock().unwrap().is_empty());
    }

    #[test]
    fn exhausted_transient_failures_have_one_enumerable_redacted_result() {
        let translator = FlakyTranslator {
            failures: usize::MAX,
            retry_reason: Some(RetryReason::ServerError),
            calls: AtomicUsize::new(0),
        };
        let sleeper = RecordingSleeper::default();

        let execution = execute_without_cache(&translator, &sleeper);
        let error = execution.outcome.unwrap_err();

        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Translation(TranslationReason::RetryExhausted)
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 4);
        assert_eq!(execution.retries.len(), 3);
        assert!(!format!("{error:?}\n{error}").contains("transient failure"));
    }

    #[test]
    fn thread_sleeper_is_a_production_sleeper() {
        let _: &dyn Sleeper = &ThreadSleeper;
    }

    #[test]
    fn translatable_echo_is_retried_once_before_accepting_a_translation() {
        let translator = ScriptedTranslator::new(["source", "translated"]);

        let execution = execute_without_cache(&translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::Translated { .. })
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_second_translatable_echo_is_accepted_after_exactly_one_retry() {
        let translator = ScriptedTranslator::new(["source", "source"]);

        let execution = execute_without_cache(&translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::Identity {
                suspicious: true,
                ..
            })
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn every_placeholder_violation_subtype_gets_one_semantic_retry() {
        let prepared = PreparedTranslation::new([
            PreparedPart::Text {
                text: "Alpha ".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
            PreparedPart::Text {
                text: " bold ".to_owned(),
                bold: true,
            },
            PreparedPart::Formula,
        ]);
        const VALID: &str = "Translated {v1}<b1> strong </b1>{v2}";
        for (invalid, violation, correction) in [
            (
                "Translated <b1> strong </b1>{v2}",
                PlaceholderViolation::Missing,
                "the previous response omitted required placeholders: {v1}. include each missing placeholder exactly once.",
            ),
            (
                "Translated {v1}{v1}<b1> strong </b1>{v2}",
                PlaceholderViolation::Duplicate,
                "the previous response duplicated placeholders: {v1}. include every required placeholder exactly once.",
            ),
            (
                "Translated {v1}<b1> strong </b1>{v2}{v3}",
                PlaceholderViolation::Unknown,
                "the previous response introduced unknown placeholders: {v3}. use only this required placeholder sequence: {v1}, <b1>, </b1>, {v2}.",
            ),
            (
                "Translated {v1}</b1> strong <b1>{v2}",
                PlaceholderViolation::TagNesting,
                "the previous response mis-nested bold placeholders. use this exact bold-tag order: <b1>, </b1>.",
            ),
            (
                "Translated {v2}<b1> strong </b1>{v1}",
                PlaceholderViolation::FormulaOrder,
                "the previous response changed formula placeholder order. use this exact formula order: {v1}, {v2}. include each exactly once.",
            ),
            (
                "Translated {v1}<b1> strong </b1>{v2",
                PlaceholderViolation::PartialToken,
                "the previous response contained a partial placeholder. emit only complete placeholders and use this required sequence: {v1}, <b1>, </b1>, {v2}.",
            ),
        ] {
            let translator = ScriptedTranslator::new([invalid, VALID]);

            let execution = execute_prepared_without_cache(
                &prepared,
                &translator,
                &RecordingSleeper::default(),
            );

            assert!(matches!(
                execution.outcome,
                Ok(TranslationOutcome::Translated { .. })
            ));
            assert_eq!(translator.calls.load(Ordering::SeqCst), 2, "{invalid}");
            assert_eq!(
                translator.placeholder_corrections(),
                [None, Some(correction.to_owned())],
                "{invalid}"
            );
            assert_eq!(
                execution.placeholder_retries,
                [PlaceholderRetryAttempt {
                    attempt: 1,
                    violation,
                }]
            );
        }
    }

    #[test]
    fn a_second_placeholder_violation_degrades_after_one_corrected_retry() {
        let prepared = PreparedTranslation::new([
            PreparedPart::Text {
                text: "Alpha ".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
            PreparedPart::Text {
                text: " omega".to_owned(),
                bold: false,
            },
        ]);
        let translator = ScriptedTranslator::new([
            "Translated without the formula",
            "Still translated without the formula",
        ]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::PlaceholderViolation {
                violation: PlaceholderViolation::Missing,
                ..
            })
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            translator.placeholder_corrections(),
            [
                None,
                Some(
                    "the previous response omitted required placeholders: {v1}. include each missing placeholder exactly once."
                        .to_owned()
                ),
            ]
        );
        assert_eq!(
            execution.placeholder_retries,
            [PlaceholderRetryAttempt {
                attempt: 1,
                violation: PlaceholderViolation::Missing,
            }]
        );
    }

    #[test]
    fn missing_conserved_content_gets_one_corrected_retry() {
        let prepared = PreparedTranslation::new([PreparedPart::Text {
            text: "At 3.5 days, latency was 20 ms.".to_owned(),
            bold: false,
        }]);
        let translator = ScriptedTranslator::new(["延迟为 20 ms。", "在 3.5 天时，延迟为 20 ms。"]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::Translated { .. })
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            translator.content_corrections(),
            [
                None,
                Some(
                    "the previous response omitted conserved source tokens: 3.5. preserve every listed token or a lexically explicit equivalent."
                        .to_owned()
                ),
            ]
        );
        assert_eq!(
            execution.content_conservation_retries,
            [ContentConservationRetryAttempt {
                attempt: 1,
                missing_token_count: 1,
                missing_tokens: vec!["3.5".to_owned()],
            }]
        );
    }

    #[test]
    fn formula_boundaries_drive_runtime_conservation_evidence() {
        let prepared = PreparedTranslation::new([
            PreparedPart::Text {
                text: "0".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
            PreparedPart::Text {
                text: "h".to_owned(),
                bold: false,
            },
        ]);
        let translator = ScriptedTranslator::new(["值0{v1}h"]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        let Ok(TranslationOutcome::Translated { conservation, .. }) = execution.outcome else {
            panic!("formula-delimited quantity should conserve");
        };
        assert_eq!(
            conservation.source_tokens,
            [crate::il::ConservedTokenCount {
                token: "0".to_owned(),
                occurrences: 1,
            }]
        );
        assert_eq!(conservation.target_tokens, conservation.source_tokens);
        assert_eq!(conservation.source_token_types, 1);
        assert_eq!(conservation.target_token_types, 1);
        assert_eq!(conservation.request_sha256.len(), 64);
        assert_eq!(conservation.response_sha256.len(), 64);
        assert_ne!(conservation.request_sha256, conservation.response_sha256);
        assert!(execution.content_conservation_retries.is_empty());
        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_second_conservation_failure_returns_an_enumerable_violation() {
        let prepared = PreparedTranslation::new([PreparedPart::Text {
            text: "Use 4 models and see [7].".to_owned(),
            bold: false,
        }]);
        let translator = ScriptedTranslator::new(["使用模型。", "仍然使用模型。。"]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::ContentConservationViolation { ref missing_tokens, .. })
                if missing_tokens == &["4".to_owned(), "[7]".to_owned()]
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 2);
        assert_eq!(execution.content_conservation_retries.len(), 1);
    }

    #[test]
    fn immutable_shapes_keep_identity_without_a_semantic_retry() {
        for source in ["12345", "person@example.com", "[] +-= 42"] {
            let prepared = PreparedTranslation::new([PreparedPart::Text {
                text: source.to_owned(),
                bold: false,
            }]);
            let translator = ScriptedTranslator::new([source]);

            let execution = execute_prepared_without_cache(
                &prepared,
                &translator,
                &RecordingSleeper::default(),
            );

            assert!(matches!(
                execution.outcome,
                Ok(TranslationOutcome::Identity {
                    suspicious: false,
                    ..
                })
            ));
            assert_eq!(translator.calls.load(Ordering::SeqCst), 1, "{source}");
        }
    }

    #[test]
    fn identity_evidence_keeps_formula_delimited_numeric_tokens_separate() {
        let prepared = PreparedTranslation::new([
            PreparedPart::Text {
                text: "5".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
            PreparedPart::Text {
                text: "1".to_owned(),
                bold: false,
            },
        ]);
        let translator = ScriptedTranslator::new(["5{v1}1"]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        let Ok(TranslationOutcome::Identity { conservation, .. }) = execution.outcome else {
            panic!("numeric formula shape should be an identity");
        };
        assert_eq!(
            conservation.source_tokens,
            [
                crate::il::ConservedTokenCount {
                    token: "1".to_owned(),
                    occurrences: 1,
                },
                crate::il::ConservedTokenCount {
                    token: "5".to_owned(),
                    occurrences: 1,
                },
            ]
        );
        assert_eq!(conservation.target_tokens, conservation.source_tokens);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn echo_and_placeholder_retries_have_independent_one_shot_budgets() {
        let prepared = PreparedTranslation::new([
            PreparedPart::Text {
                text: "Alpha ".to_owned(),
                bold: false,
            },
            PreparedPart::Formula,
        ]);
        let translator =
            ScriptedTranslator::new(["Alpha {v1}", "Translated {v999}", "Translated {v1}"]);

        let execution =
            execute_prepared_without_cache(&prepared, &translator, &RecordingSleeper::default());

        assert!(matches!(
            execution.outcome,
            Ok(TranslationOutcome::Translated { .. })
        ));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            execution.placeholder_retries,
            [PlaceholderRetryAttempt {
                attempt: 2,
                violation: PlaceholderViolation::Unknown,
            }]
        );
    }
}
