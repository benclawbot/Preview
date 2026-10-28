use serde::Serialize;

/// Every error that can cross the Tauri IPC boundary.
///
/// The original error is flattened to a human-readable message; the frontend
/// only ever needs something to show in a banner or dialog.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),

    #[error("This feature is not part of the MVP: {0}")]
    Unsupported(String),

    #[error("document not found")]
    DocumentNotFound,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Pdf(#[from] lopdf::Error),

    #[error(transparent)]
    Pdfium(#[from] pdfium_render::prelude::PdfiumError),

    #[error(transparent)]
    Image(#[from] image::ImageError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<String> for AppError {
    fn from(value: String) -> Self {
        AppError::Message(value)
    }
}

impl From<&str> for AppError {
    fn from(value: &str) -> Self {
        AppError::Message(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
