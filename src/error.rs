//! The one install failure printed from `main`.

use std::fmt;
use std::io;
use std::path::Path;

#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn io(action: &str, path: &Path, source: io::Error) -> Self {
        Self::new(format!("cannot {action} {}: {source}", path.display()))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
