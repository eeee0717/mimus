use crate::error::{MimusError, Result, TranslationReason};

pub trait Translator: Send + Sync {
    fn translate(&self, text: &str) -> Result<String>;
}

#[derive(Debug, Default)]
pub struct NoneTranslator;

impl Translator for NoneTranslator {
    fn translate(&self, text: &str) -> Result<String> {
        Ok(text.to_owned())
    }
}

#[must_use]
pub fn openai_not_implemented() -> MimusError {
    MimusError::translation(
        TranslationReason::BackendNotImplemented,
        "the openai backend is not implemented in M1",
    )
    .with_hint("rerun with --backend none for an offline round trip")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_translator_is_an_offline_identity_adapter() {
        assert_eq!(NoneTranslator.translate("MIMUS").unwrap(), "MIMUS");
    }
}
