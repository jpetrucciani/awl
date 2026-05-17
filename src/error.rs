use thiserror::Error;

pub type Result<T> = std::result::Result<T, AwlError>;

#[derive(Debug, Error)]
pub enum AwlError {
    #[error("{message}")]
    Usage { message: String },
    #[error("unsupported command in this implementation slice: {command}")]
    Unsupported { command: String },
    #[error("not found: {message}")]
    NotFound { message: String },
    #[error("authentication failed: {message}")]
    Authentication { message: String },
    #[error("authorization failed: {message}")]
    Authorization { message: String },
    #[error("endpoint error: {message}")]
    Endpoint { message: String },
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
            AwlError::Usage { .. } => 2,
            AwlError::Authentication { .. } => 3,
            AwlError::Authorization { .. } => 4,
            AwlError::NotFound { .. } => 5,
            AwlError::Endpoint { .. } => 8,
            AwlError::Unsupported { .. }
            | AwlError::Io(_)
            | AwlError::Json(_)
            | AwlError::Yaml(_)
            | AwlError::Toml(_)
            | AwlError::Aws { .. } => 1,
        }
    }

    pub fn aws(error: impl std::fmt::Display) -> Self {
        Self::Aws {
            message: error.to_string(),
        }
    }
}
