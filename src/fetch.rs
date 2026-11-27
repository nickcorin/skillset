//! Getting a repository snapshot for a pinned commit.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::lock::{CommitSha, GitHubRepo};
use crate::snapshot::{ArchiveError, RepoSnapshot};

const CODELOAD_URL: &str = "https://codeload.github.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Provides a repository's files at one commit.
pub trait SourceFetcher {
    fn fetch(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<RepoSnapshot, FetchError>;
}

/// Downloads `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`, the
/// archive GitHub serves for a commit, and decodes it while it streams in.
pub struct GitHubTarballFetcher {
    agent: ureq::Agent,
}

impl GitHubTarballFetcher {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(concat!("skillset/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }

    pub fn tarball_url(repo: &GitHubRepo, commit: &CommitSha) -> String {
        format!(
            "{CODELOAD_URL}/{}/{}/tar.gz/{commit}",
            repo.owner(),
            repo.repo()
        )
    }
}

impl Default for GitHubTarballFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceFetcher for GitHubTarballFetcher {
    fn fetch(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<RepoSnapshot, FetchError> {
        let url = Self::tarball_url(repo, commit);
        let response = self.agent.get(&url).call().map_err(|err| match err {
            ureq::Error::StatusCode(status) => FetchError::Status {
                url: url.clone(),
                status,
            },
            other => FetchError::Transport {
                url: url.clone(),
                source: Box::new(other),
            },
        })?;
        RepoSnapshot::from_tar_gz(response.into_body().into_reader())
            .map_err(|source| FetchError::Archive { url, source })
    }
}

/// Why a snapshot could not be fetched.
#[derive(Debug)]
pub enum FetchError {
    /// The server answered with a non-success status.
    Status { url: String, status: u16 },
    /// No response arrived.
    Transport {
        url: String,
        source: Box<dyn Error + Send + Sync>,
    },
    /// The response was not a readable source archive.
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
            Self::Transport { url, source } => write!(f, "request to {url} failed: {source}"),
            Self::Archive { url, source } => write!(f, "{url}: {source}"),
        }
    }
}

impl Error for FetchError {}

#[cfg(test)]
mod tests {
    use super::GitHubTarballFetcher;
    use crate::lock::{CommitSha, GitHubRepo};

    #[test]
    fn tarball_url_pins_the_commit() {
        let repo = GitHubRepo::parse("vercel-labs/skills").unwrap();
        let commit = CommitSha::parse("e7fd0e9e52f029fe87729d8185cb8caf7049f678").unwrap();
        assert_eq!(
            GitHubTarballFetcher::tarball_url(&repo, &commit),
            "https://codeload.github.com/vercel-labs/skills/tar.gz/e7fd0e9e52f029fe87729d8185cb8caf7049f678"
        );
    }
}
