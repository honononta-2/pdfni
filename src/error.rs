#[cfg(feature = "pdfium")]
use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};
use thiserror::Error;

/// 表抽出のエラー
#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to load pdfium library: {0}")]
    PdfiumBind(String),

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

    #[error("pdfium error: {0}")]
    Pdfium(String),

    #[error("pure Rust reader error: {0}")]
    ReaderRs(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(feature = "pdfium")]
impl From<PdfiumError> for Error {
    fn from(e: PdfiumError) -> Self {
        match e {
            #[cfg(not(target_arch = "wasm32"))]
            PdfiumError::LoadLibraryError(err) => Error::PdfiumBind(format!("{err}")),
            #[cfg(not(target_arch = "wasm32"))]
            PdfiumError::LoadLibraryFunctionNameError(s) => Error::PdfiumBind(s),
            PdfiumError::IoError(err) => Error::PdfOpen(err),
            PdfiumError::PdfiumLibraryInternalError(internal) => match internal {
                PdfiumInternalError::PasswordError => Error::BadPassword,
                PdfiumInternalError::SecurityError => Error::SecurityRestricted,
                PdfiumInternalError::FormatError => Error::InvalidFormat,
                PdfiumInternalError::FileError => {
                    Error::PdfOpen(std::io::Error::other("FPDF_ERR_FILE"))
                }
                PdfiumInternalError::PageError => Error::PageRead("FPDF_ERR_PAGE".into()),
                PdfiumInternalError::Unknown => Error::Pdfium("FPDF_ERR_UNKNOWN".into()),
            },
            other => Error::Pdfium(format!("{other:?}")),
        }
    }
}
