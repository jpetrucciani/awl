use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AwlError>;

#[derive(Debug, Error)]
pub enum AwlError {
    #[error("{message}")]
    Usage { message: String },
    #[error("not found: {message}")]
    NotFound { message: String },
    #[error("authentication failed: {message}")]
    Authentication { message: String },
    #[error("authorization failed: {message}")]
    Authorization { message: String },
    #[error("throttled: {message}")]
    Throttled { message: String },
    #[error("endpoint error: {message}")]
    Endpoint { message: String },
    #[error("missing dependency: {message}")]
    MissingDependency { message: String },
    #[error("dry run: {message}")]
    DryRun { message: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("aws error: {message}")]
    Aws { message: String },
}

impl AwlError {
    pub fn exit_code(&self) -> u8 {
        match self {
            AwlError::DryRun { .. } => 0,
            AwlError::Usage { .. } => 2,
            AwlError::Authentication { .. } => 3,
            AwlError::Authorization { .. } => 4,
            AwlError::NotFound { .. } => 5,
            AwlError::Throttled { .. } => 7,
            AwlError::Endpoint { .. } => 8,
            AwlError::MissingDependency { .. } => 9,
            AwlError::Io(_)
            | AwlError::Json(_)
            | AwlError::Yaml(_)
            | AwlError::Toml(_)
            | AwlError::Aws { .. } => 1,
        }
    }

    /// True when stdout was closed by the reader (for example `awl ... | head`).
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, AwlError::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
    }

    /// Wrap a non-request error (builders, byte streams, presigning config).
    pub fn aws(error: impl std::fmt::Display) -> Self {
        Self::Aws {
            message: error.to_string(),
        }
    }

    /// Classify an AWS SDK request error by its service error code so exit codes
    /// mean something, and keep the code and message in the rendered error.
    pub fn sdk<E>(error: E) -> Self
    where
        E: ProvideErrorMetadata + std::error::Error,
    {
        let Some(code) = error.code().map(str::to_owned) else {
            let message = error_chain(&error);
            let lower = message.to_lowercase();
            return if lower.contains("credential") {
                Self::Authentication { message }
            } else if lower.contains("dispatch failure") || lower.contains("timeout") {
                Self::Endpoint { message }
            } else {
                Self::Aws { message }
            };
        };
        let message = match error.message() {
            Some(detail) if !detail.is_empty() => format!("{code}: {detail}"),
            _ => code.clone(),
        };
        match code.as_str() {
            "DryRunOperation" => Self::DryRun { message },
            "AccessDenied"
            | "AccessDeniedException"
            | "AuthorizationError"
            | "Forbidden"
            | "UnauthorizedOperation" => Self::Authorization { message },
            "ExpiredToken"
            | "ExpiredTokenException"
            | "InvalidAccessKeyId"
            | "InvalidClientTokenId"
            | "InvalidToken"
            | "SignatureDoesNotMatch"
            | "UnrecognizedClientException" => Self::Authentication { message },
            "Throttling"
            | "ThrottlingException"
            | "TooManyRequestsException"
            | "RequestLimitExceeded"
            | "SlowDown"
            | "ProvisionedThroughputExceededException" => Self::Throttled { message },
            code if code == "NotFound"
                || code.starts_with("NoSuch")
                || code.ends_with("NotFound")
                || code.ends_with("NotFoundException")
                || code.contains("NonExistent")
                || code.ends_with("DoesNotExist") =>
            {
                Self::NotFound { message }
            }
            _ => Self::Aws { message },
        }
    }
}

/// `outer: inner: root` without the Debug dump DisplayErrorContext appends.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        let part = error.to_string();
        if !parts.iter().any(|seen| seen.contains(&part)) {
            parts.push(part);
        }
        source = error.source();
    }
    parts.join(": ")
}
