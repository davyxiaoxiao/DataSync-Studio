use serde::{Serialize, Serializer};

/// Unified error type for every Tauri command.
///
/// `sqlx::Error` is deliberately rendered as a flat message: the frontend only
/// shows it inside the log panel, and the source chain is usually noise there.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),

    #[error("{0}")]
    Driver(#[from] sqlx::Error),

    #[error("{0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn msg(message: impl Into<String>) -> Self {
        AppError::Message(message.into())
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
