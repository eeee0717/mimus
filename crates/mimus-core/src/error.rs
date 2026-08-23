use std::fmt;

use serde::{Serialize, Serializer};
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

macro_rules! reason_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }
    };
}

reason_enum!(UsageReason {
    InvalidArguments => "invalid_arguments",
});

reason_enum!(InputReason {
    InputRead => "input_read",
    PdfParse => "pdf_parse",
    EncryptedPdf => "encrypted_pdf",
    ScannedPdf => "scanned_pdf",
    UnsupportedPdf => "unsupported_pdf",
    OperatorWalk => "operator_walk",
    EngineMismatch => "engine_mismatch",
    OutputMismatch => "output_mismatch",
    OutputWrite => "output_write",
    AtomicPublish => "atomic_publish",
});

reason_enum!(AssetReason {
    PdfiumUnavailable => "pdfium_unavailable",
});

reason_enum!(TranslationReason {
    BackendNotImplemented => "backend_not_implemented",
    TranslationFailed => "translation_failed",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReason {
    Usage(UsageReason),
    Input(InputReason),
    Asset(AssetReason),
    Translation(TranslationReason),
}

impl ErrorReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage(reason) => reason.as_str(),
            Self::Input(reason) => reason.as_str(),
            Self::Asset(reason) => reason.as_str(),
            Self::Translation(reason) => reason.as_str(),
        }
    }
}

impl fmt::Display for ErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ErrorReason {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Error)]
pub enum MimusError {
    #[error("{message}")]
    Usage {
        reason: UsageReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Input {
        reason: InputReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Asset {
        reason: AssetReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Translation {
        reason: TranslationReason,
        message: String,
        hint: Option<String>,
    },
}

impl MimusError {
    #[must_use]
    pub fn usage(reason: UsageReason, message: impl Into<String>) -> Self {
        Self::Usage {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn input(reason: InputReason, message: impl Into<String>) -> Self {
        Self::Input {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn asset(reason: AssetReason, message: impl Into<String>) -> Self {
        Self::Asset {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn translation(reason: TranslationReason, message: impl Into<String>) -> Self {
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
            Self::Usage { reason, .. } => ErrorReason::Usage(*reason),
            Self::Input { reason, .. } => ErrorReason::Input(*reason),
            Self::Asset { reason, .. } => ErrorReason::Asset(*reason),
            Self::Translation { reason, .. } => ErrorReason::Translation(*reason),
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
    fn typed_reasons_keep_category_and_wire_value_together() {
        let error = MimusError::translation(
            TranslationReason::BackendNotImplemented,
            "openai is not implemented",
        )
        .with_hint("use --backend none");
        assert_eq!(error.category(), ExitCategory::Translation);
        assert_eq!(
            error.reason(),
            ErrorReason::Translation(TranslationReason::BackendNotImplemented)
        );
        assert_eq!(error.hint(), Some("use --backend none"));
        assert_eq!(error.reason().to_string(), "backend_not_implemented");
        assert_eq!(
            serde_json::to_string(&error.reason()).unwrap(),
            "\"backend_not_implemented\""
        );
    }
}
