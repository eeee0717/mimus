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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryAttempt {
    pub attempt: usize,
    pub delay_ms: u64,
    pub reason: RetryReason,
}

#[derive(Debug)]
pub(crate) enum TranslationOutcome {
    Translated(RestoredTranslation),
    Identity,
    PlaceholderViolation {
        violation: PlaceholderViolation,
        profile: RedactedTranslationProfile,
    },
}

pub(crate) struct TranslationExecution {
    pub cache_status: Option<CacheStatus>,
    pub retries: Vec<RetryAttempt>,
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
                    outcome: Ok(TranslationOutcome::Identity),
                };
            }
            Ok(Some(CachedTranslation::Translated(validated))) => {
                if let Ok(restored) = request.prepared.restore(&validated) {
                    return TranslationExecution {
                        cache_status: Some(CacheStatus::Hit),
                        retries: Vec::new(),
                        outcome: Ok(TranslationOutcome::Translated(restored)),
                    };
                }
                Some(CacheStatus::Miss)
            }
            Ok(None) => Some(CacheStatus::Miss),
            Err(error) => {
                return TranslationExecution {
                    cache_status: None,
                    retries: Vec::new(),
                    outcome: Err(error),
                };
            }
        }
    } else {
        None
    };

    let (output, retries) = translate_with_retry(
        translator,
        sleeper,
        &TranslationRequest {
            text: request.prepared.request_text(),
            target_language: request.target_language,
            glossary: request.glossary,
        },
    );
    let outcome = output.and_then(|output| match request.prepared.classify(&output) {
        super::TranslationOutcome::Identity => {
            if let Some(cache) = cache {
                cache.insert_identity(request.cache_key)?;
            }
            Ok(TranslationOutcome::Identity)
        }
        super::TranslationOutcome::Translated(validated) => {
            match request.prepared.restore(&validated) {
                Ok(restored) => {
                    if let Some(cache) = cache {
                        cache.insert(request.cache_key, &validated)?;
                    }
                    Ok(TranslationOutcome::Translated(restored))
                }
                Err(violation) => Ok(TranslationOutcome::PlaceholderViolation {
                    violation,
                    profile: super::redacted_translation_profile(&output),
                }),
            }
        }
        super::TranslationOutcome::PlaceholderViolation(violation) => {
            Ok(TranslationOutcome::PlaceholderViolation {
                violation,
                profile: super::redacted_translation_profile(&output),
            })
        }
    });
    TranslationExecution {
        cache_status,
        retries,
        outcome,
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
        let glossary = Glossary::default();
        execute(
            translator,
            None,
            sleeper,
            ExecutionRequest {
                prepared: &prepared,
                target_language: "zh-CN",
                glossary: &glossary,
                cache_key: &TranslationCacheKey::new(
                    "source", "model", "zh-CN", "prompt", "glossary",
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
                Ok(TranslationOutcome::Translated(_))
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
}
