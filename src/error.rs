use thiserror::Error;

/// 表抽出のエラー
#[derive(Debug, Error)]
pub enum Error {
    #[error("PDF password is required or incorrect")]
    BadPassword,

    #[error("PDF security restrictions prevent loading")]
    SecurityRestricted,

    #[error("invalid PDF format")]
    InvalidFormat,

    #[error("failed to open PDF file: {0}")]
    PdfOpen(#[source] std::io::Error),

    #[error("page read failed: {0}")]
    PageRead(String),

    #[error("pure Rust reader error: {0}")]
    Reader(String),
}

pub type Result<T> = std::result::Result<T, Error>;
