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
    Io,
    Internal,
}

impl ExitCategory {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Usage => 1,
            Self::Input => 2,
            Self::Asset => 3,
            Self::Translation => 4,
            Self::Io => 5,
            Self::Internal => 6,
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
    PdfParse => "pdf_parse",
    EncryptedPdf => "encrypted_pdf",
    ScannedPdf => "scanned_pdf",
    UnsupportedPdf => "unsupported_pdf",
    OperatorWalk => "operator_walk",
    EngineMismatch => "engine_mismatch",
});

reason_enum!(AssetReason {
    PdfiumUnavailable => "pdfium_unavailable",
});

reason_enum!(TranslationReason {
    BackendNotImplemented => "backend_not_implemented",
    TranslationFailed => "translation_failed",
});

reason_enum!(IoReason {
    InputRead => "input_read",
    OutputWrite => "output_write",
    AtomicPublish => "atomic_publish",
    DebugWrite => "debug_write",
    StdoutWrite => "stdout_write",
});

reason_enum!(InternalReason {
    OutputBuild => "output_build",
    OutputMismatch => "output_mismatch",
    EventSerialization => "event_serialization",
    InvariantViolation => "invariant_violation",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorReason {
    Usage(UsageReason),
    Input(InputReason),
    Asset(AssetReason),
    Translation(TranslationReason),
    Io(IoReason),
    Internal(InternalReason),
}

impl ErrorReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage(reason) => reason.as_str(),
            Self::Input(reason) => reason.as_str(),
            Self::Asset(reason) => reason.as_str(),
            Self::Translation(reason) => reason.as_str(),
            Self::Io(reason) => reason.as_str(),
            Self::Internal(reason) => reason.as_str(),
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
    #[error("{message}")]
    Io {
        reason: IoReason,
        message: String,
        hint: Option<String>,
    },
    #[error("{message}")]
    Internal {
        reason: InternalReason,
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
    pub fn io(reason: IoReason, message: impl Into<String>) -> Self {
        Self::Io {
            reason,
            message: message.into(),
            hint: None,
        }
    }

    #[must_use]
    pub fn internal(reason: InternalReason, message: impl Into<String>) -> Self {
        Self::Internal {
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
            | Self::Translation { hint, .. }
            | Self::Io { hint, .. }
            | Self::Internal { hint, .. } => *hint = Some(value.into()),
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
            Self::Io { .. } => ExitCategory::Io,
            Self::Internal { .. } => ExitCategory::Internal,
        }
    }

    #[must_use]
    pub const fn reason(&self) -> ErrorReason {
        match self {
            Self::Usage { reason, .. } => ErrorReason::Usage(*reason),
            Self::Input { reason, .. } => ErrorReason::Input(*reason),
            Self::Asset { reason, .. } => ErrorReason::Asset(*reason),
            Self::Translation { reason, .. } => ErrorReason::Translation(*reason),
            Self::Io { reason, .. } => ErrorReason::Io(*reason),
            Self::Internal { reason, .. } => ErrorReason::Internal(*reason),
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Usage { hint, .. }
            | Self::Input { hint, .. }
            | Self::Asset { hint, .. }
            | Self::Translation { hint, .. }
            | Self::Io { hint, .. }
            | Self::Internal { hint, .. } => hint.as_deref(),
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
        assert_eq!(ExitCategory::Io.code(), 5);
        assert_eq!(ExitCategory::Internal.code(), 6);
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
