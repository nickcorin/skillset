//! Where a repository's files at one commit come from.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::repository::{ArchiveError, Repository};

const CODELOAD_URL: &str = "https://codeload.github.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// `owner/repo` as `npx skills` accepts it, with an optional `.git` suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRepo {
    owner: String,
    name: String,
}

impl GitHubRepo {
    pub(crate) fn parse(value: &str) -> Result<Self, GitHubRepoError> {
        let Some((owner, name)) = value.split_once('/') else {
            return Err(GitHubRepoError {
                value: value.to_owned(),
            });
        };
        let name = name.strip_suffix(".git").unwrap_or(name);
        let owner_ok = !owner.is_empty()
            && owner
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        let name_ok = !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if owner_ok && name_ok {
            Ok(Self {
                owner: owner.to_owned(),
                name: name.to_owned(),
            })
        } else {
            Err(GitHubRepoError {
                value: value.to_owned(),
            })
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// `source` was not `owner/repo`.
#[derive(Debug)]
pub struct GitHubRepoError {
    value: String,
}

impl fmt::Display for GitHubRepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "has invalid \"source\" {:?} (expected owner/repo)",
            self.value
        )
    }
}

impl Error for GitHubRepoError {}

/// A full 40-character commit SHA. Uppercase hex is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSha(String);

impl CommitSha {
    pub(crate) fn parse(value: &str) -> Result<Self, CommitShaError> {
        if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CommitShaError {
                value: value.to_owned(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `commit` was not a full SHA.
#[derive(Debug)]
pub struct CommitShaError {
    value: String,
}

impl fmt::Display for CommitShaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "has invalid \"commit\" {:?} (expected a full 40-character hex commit SHA)",
            self.value
        )
    }
}

impl Error for CommitShaError {}

/// The repository a skill is taken from.
pub trait Source {
    type Error: Error + Send + Sync + 'static;

    fn repository(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<Repository, Self::Error>;
}

/// Downloads `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`.
pub struct GitHub {
    agent: ureq::Agent,
}

impl GitHub {
    /// An HTTP client for `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>`.
    pub fn codeload() -> Self {
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

    fn repository(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<Repository, FetchError> {
        let url = format!(
            "{CODELOAD_URL}/{}/{}/tar.gz/{}",
            repo.owner(),
            repo.name(),
            commit.as_str()
        );
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
        Repository::from_tar_gz(response.into_body().into_reader())
            .map_err(|source| FetchError::Archive { url, source })
    }
}

/// Why a repository could not be fetched.
#[derive(Debug)]
pub enum FetchError {
    Status {
        url: String,
        status: u16,
    },
    Transport {
        url: String,
        source: Box<ureq::Error>,
    },
    Archive {
        url: String,
        source: ArchiveError,
    },
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

impl Error for FetchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Status { .. } => None,
            Self::Transport { source, .. } => Some(source.as_ref()),
            Self::Archive { source, .. } => Some(source),
        }
    }
}
