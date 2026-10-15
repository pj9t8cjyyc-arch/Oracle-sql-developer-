use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(String),
    #[error("blocked by safe mode: {0}")]
    Blocked(String),
    #[error("unknown query handle {0}")]
    UnknownHandle(u64),
    #[error("invalid input: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;
