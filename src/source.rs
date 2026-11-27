//! Where a repository's files at one commit come from.

use std::fmt;
use std::time::Duration;

use crate::repository::{ArchiveError, Repository};

const CODELOAD_URL: &str = "https://codeload.github.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The repository a skill is taken from.
pub trait Source {
    type Error: fmt::Display + Send + Sync + 'static;

    fn repository(&self, owner: &str, repo: &str, commit: &str) -> Result<Repository, Self::Error>;
}

/// Downloads `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`.
pub struct GitHub {
    agent: ureq::Agent,
}

impl Default for GitHub {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHub {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(concat!("skillset/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

impl Source for GitHub {
    type Error = FetchError;

    fn repository(&self, owner: &str, repo: &str, commit: &str) -> Result<Repository, FetchError> {
        let url = format!("{CODELOAD_URL}/{owner}/{repo}/tar.gz/{commit}");
        let response = self.agent.get(&url).call().map_err(|err| match err {
            ureq::Error::StatusCode(status) => FetchError::Status {
                url: url.clone(),
                status,
            },
            other => FetchError::Transport {
                url: url.clone(),
                reason: other.to_string(),
            },
        })?;
        Repository::from_tar_gz(response.into_body().into_reader())
            .map_err(|source| FetchError::Archive { url, source })
    }
}

/// Why a repository could not be fetched.
#[derive(Debug)]
pub enum FetchError {
    Status { url: String, status: u16 },
    Transport { url: String, reason: String },
    Archive { url: String, source: ArchiveError },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status { url, status: 404 } => write!(
                f,
                "{url} returned HTTP 404 (the repository or commit does not exist, or the repository is private)"
            ),
            Self::Status { url, status } => write!(f, "{url} returned HTTP {status}"),
            Self::Transport { url, reason } => write!(f, "request to {url} failed: {reason}"),
            Self::Archive { url, source } => write!(f, "{url}: {source}"),
        }
    }
}

impl std::error::Error for FetchError {}
