use std::fmt;

use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MimusError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitCategory {
    Usage,
    Input,
    Asset,
    Translation,
}

impl ExitCategory {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Usage => 1,
            Self::Input => 2,
            Self::Asset => 3,
            Self::Translation => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorReason {
    InvalidArguments,
    InputRead,
    PdfParse,
    EncryptedPdf,
    ScannedPdf,
    UnsupportedPdf,
    OperatorWalk,
    EngineMismatch,
    OutputWrite,
    AtomicPublish,
    PdfiumUnavailable,
    BackendNotImplemented,
    TranslationFailed,
}

impl fmt::Display for ErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = serde_json_name(*self);
        formatter.write_str(value)
    }
}

const fn serde_json_name(reason: ErrorReason) -> &'static str {
    match reason {
        ErrorReason::InvalidArguments => "invalid_arguments",
        ErrorReason::InputRead => "input_read",
        ErrorReason::PdfParse => "pdf_parse",
        ErrorReason::EncryptedPdf => "encrypted_pdf",
        ErrorReason::ScannedPdf => "scanned_pdf",
        ErrorReason::UnsupportedPdf => "unsupported_pdf",
        ErrorReason::OperatorWalk => "operator_walk",
        ErrorReason::EngineMismatch => "engine_mismatch",
        ErrorReason::OutputWrite => "output_write",
        ErrorReason::AtomicPublish => "atomic_publish",
        ErrorReason::PdfiumUnavailable => "pdfium_unavailable",
        ErrorReason::BackendNotImplemented => "backend_not_implemented",
        ErrorReason::TranslationFailed => "translation_failed",
    }
}

#[derive(Debug, Error)]
pub enum MimusError {
    #[error("{message}")]
    Usage {
        reason: ErrorReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Input {
        reason: ErrorReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Asset {
        reason: ErrorReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Translation {
        reason: ErrorReason,
        message: String,
        hint: Option<String>,
    },
}

impl MimusError {
    #[must_use]
    pub fn usage(reason: ErrorReason, message: impl Into<String>) -> Self {
        Self::Usage {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn input(reason: ErrorReason, message: impl Into<String>) -> Self {
        Self::Input {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn asset(reason: ErrorReason, message: impl Into<String>) -> Self {
        Self::Asset {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn translation(reason: ErrorReason, message: impl Into<String>) -> Self {
        Self::Translation {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn with_hint(mut self, value: impl Into<String>) -> Self {
        match &mut self {
            Self::Usage { hint, .. }
            | Self::Input { hint, .. }
            | Self::Asset { hint, .. }
            | Self::Translation { hint, .. } => *hint = Some(value.into()),
        }
        self
    }

    #[must_use]
    pub const fn category(&self) -> ExitCategory {
        match self {
            Self::Usage { .. } => ExitCategory::Usage,
            Self::Input { .. } => ExitCategory::Input,
            Self::Asset { .. } => ExitCategory::Asset,
            Self::Translation { .. } => ExitCategory::Translation,
        }
    }

    #[must_use]
    pub const fn reason(&self) -> ErrorReason {
        match self {
            Self::Usage { reason, .. }
            | Self::Input { reason, .. }
            | Self::Asset { reason, .. }
            | Self::Translation { reason, .. } => *reason,
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Usage { hint, .. }
            | Self::Input { hint, .. }
            | Self::Asset { hint, .. }
            | Self::Translation { hint, .. } => hint.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_categories_keep_the_public_codes() {
        assert_eq!(ExitCategory::Usage.code(), 1);
        assert_eq!(ExitCategory::Input.code(), 2);
        assert_eq!(ExitCategory::Asset.code(), 3);
        assert_eq!(ExitCategory::Translation.code(), 4);
    }

    #[test]
    fn errors_expose_enumerable_reasons_and_hints() {
        let error = MimusError::translation(
            ErrorReason::BackendNotImplemented,
            "openai is not implemented",
        )
        .with_hint("use --backend none");
        assert_eq!(error.category(), ExitCategory::Translation);
        assert_eq!(error.reason(), ErrorReason::BackendNotImplemented);
        assert_eq!(error.hint(), Some("use --backend none"));
        assert_eq!(error.reason().to_string(), "backend_not_implemented");
    }
}
