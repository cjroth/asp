//! ASP error type.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AspError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("bad signature: {0}")]
    BadSignature(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("auth denied: {0}")]
    AuthDenied(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(String),
}

pub type AspResult<T> = Result<T, AspError>;

#[cfg(not(target_arch = "wasm32"))]
impl From<rusqlite::Error> for AspError {
    fn from(e: rusqlite::Error) -> Self {
        AspError::Storage(e.to_string())
    }
}

impl From<std::io::Error> for AspError {
    fn from(e: std::io::Error) -> Self {
        AspError::Io(e.to_string())
    }
}
