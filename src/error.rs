use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    Lockfile {
        path: PathBuf,
        source: io::Error,
    },
    UnsupportedLock(String),
    Download {
        skill: String,
        message: String,
    },
    SkillNotFound {
        skill: String,
        path: String,
    },
    Symlink {
        skill: String,
        path: String,
    },
    HashMismatch {
        skill: String,
        expected: String,
        actual: String,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Lockfile { path, source } if source.kind() == io::ErrorKind::NotFound => {
                write!(formatter, "lock file not found: {}", path.display())
            }
            Error::Lockfile { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Error::UnsupportedLock(message) => {
                write!(formatter, "unsupported lock file: {message}")
            }
            Error::Download { skill, message } => {
                write!(formatter, "skill {skill} download failed: {message}")
            }
            Error::SkillNotFound { skill, path } => {
                write!(formatter, "skill {skill} not found at {path}")
            }
            Error::Symlink { skill, path } => {
                write!(
                    formatter,
                    "skill {skill} contains an unsupported symlink {path}"
                )
            }
            Error::HashMismatch {
                skill,
                expected,
                actual,
            } => write!(
                formatter,
                "skill {skill} hash mismatch: expected {expected}, actual {actual}"
            ),
            Error::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {}
