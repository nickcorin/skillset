//! A version 2 `skills-lock.json`: GitHub entries, plus an optional `skillset.installDir`
//! that `npx skills` does not read.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::content_hash::ContentHash;
use crate::skill::{SkillPath, SkillPathError};
use crate::source::{CommitSha, CommitShaError, GitHubRepo, GitHubRepoError};

/// A parsed lock file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lockfile {
    /// `skillset.installDir`, relative to the lock file's folder.
    install_dir: Option<PathBuf>,
    /// Entries in skill-name order.
    entries: Vec<LockEntry>,
}

/// One GitHub skill at a pinned commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    name: String,
    folder: FolderName,
    repo: GitHubRepo,
    commit: CommitSha,
    skill_path: SkillPath,
    hash: ContentHash,
}

/// Install folder name derived by `npx skills` `sanitizeName`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FolderName(String);

impl FolderName {
    fn from_skill_name(name: &str) -> Self {
        let mut sanitized = String::with_capacity(name.len());
        let mut in_replaced_run = false;
        for ch in name.to_lowercase().chars() {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' {
                sanitized.push(ch);
                in_replaced_run = false;
            } else if !in_replaced_run {
                sanitized.push('-');
                in_replaced_run = true;
            }
        }
        let trimmed = sanitized.trim_matches(['.', '-']);
        let truncated = &trimmed[..trimmed.len().min(255)];
        Self(if truncated.is_empty() {
            "unnamed-skill".to_owned()
        } else {
            truncated.to_owned()
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FolderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self, LockError> {
        let json = fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&json).map_err(|source| LockError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    pub(crate) fn install_dir(&self) -> Option<&Path> {
        self.install_dir.as_deref()
    }

    pub(crate) fn entries(&self) -> &[LockEntry] {
        &self.entries
    }

    fn from_json(json: &str) -> Result<Self, LockParseError> {
        let raw: RawLock =
            serde_json::from_str(json).map_err(|err| LockParseError::Json(Box::new(err)))?;
        if raw.version != 2 {
            return Err(LockParseError::Version { found: raw.version });
        }
        let install_dir = match raw.skillset {
            Some(settings) if settings.install_dir.is_empty() => {
                return Err(LockParseError::EmptyInstallDir);
            }
            Some(settings) => Some(PathBuf::from(settings.install_dir)),
            None => None,
        };

        let mut entries = Vec::with_capacity(raw.skills.len());
        let mut folders = BTreeSet::new();
        for (name, entry) in raw.skills {
            let entry = LockEntry::from_raw(name, entry)?;
            if !folders.insert(entry.folder.clone()) {
                return Err(LockParseError::SharedFolder {
                    folder: entry.folder,
                });
            }
            entries.push(entry);
        }
        Ok(Self {
            install_dir,
            entries,
        })
    }
}

impl LockEntry {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn folder(&self) -> &FolderName {
        &self.folder
    }

    pub(crate) fn repo(&self) -> &GitHubRepo {
        &self.repo
    }

    pub(crate) fn commit(&self) -> &CommitSha {
        &self.commit
    }

    pub(crate) fn skill_path(&self) -> &SkillPath {
        &self.skill_path
    }

    pub(crate) fn hash(&self) -> &ContentHash {
        &self.hash
    }

    fn from_raw(name: String, raw: RawEntry) -> Result<Self, LockParseError> {
        if let Some(skill_name) = raw.skill_name {
            return Err(LockParseError::entry(
                name,
                EntryError::Renamed { skill_name },
            ));
        }
        let repo = GitHubRepo::parse(&raw.source).map_err(|source| {
            LockParseError::entry(name.clone(), EntryError::Repository(source))
        })?;
        let commit = CommitSha::parse(&raw.commit)
            .map_err(|source| LockParseError::entry(name.clone(), EntryError::Commit(source)))?;
        let skill_path = SkillPath::parse(&raw.skill_path)
            .map_err(|source| LockParseError::entry(name.clone(), EntryError::SkillPath(source)))?;
        let Some(hash) = ContentHash::parse(&raw.computed_hash) else {
            return Err(LockParseError::entry(
                name,
                EntryError::Hash {
                    value: raw.computed_hash,
                },
            ));
        };
        Ok(Self {
            folder: FolderName::from_skill_name(&name),
            name,
            repo,
            commit,
            skill_path,
            hash,
        })
    }
}

#[derive(Deserialize)]
struct RawLock {
    version: u64,
    skills: BTreeMap<String, RawEntry>,
    skillset: Option<RawSkillset>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawSkillset {
    install_dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    source: String,
    /// Kept so serde rejects a `sourceType` other than `github`.
    #[expect(dead_code)]
    source_type: SourceKind,
    commit: String,
    skill_path: String,
    computed_hash: String,
    #[serde(default)]
    skill_name: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SourceKind {
    Github,
}

/// Why a lock file could not be read.
#[derive(Debug)]
pub enum LockError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: Box<LockParseError>,
    },
}

#[derive(Debug)]
pub enum LockParseError {
    Json(Box<serde_json::Error>),
    Version { found: u64 },
    EmptyInstallDir,
    SharedFolder { folder: FolderName },
    Entry { name: String, source: EntryError },
}

impl LockParseError {
    fn entry(name: String, source: EntryError) -> Self {
        Self::Entry { name, source }
    }
}

#[derive(Debug)]
pub enum EntryError {
    Renamed { skill_name: String },
    Repository(GitHubRepoError),
    Commit(CommitShaError),
    SkillPath(SkillPathError),
    Hash { value: String },
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl Error for LockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source.as_ref()),
        }
    }
}

impl fmt::Display for LockParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(source) => write!(f, "not a valid skills lock file: {source}"),
            Self::Version { found } => {
                write!(f, "unsupported lock version {found} (expected 2)")
            }
            Self::EmptyInstallDir => f.write_str("\"skillset.installDir\" is empty"),
            Self::SharedFolder { folder } => write!(
                f,
                "more than one skill would install into folder \"{folder}\""
            ),
            Self::Entry { name, source } => write!(f, "skill \"{name}\" {source}"),
        }
    }
}

impl Error for LockParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(source) => Some(source.as_ref()),
            Self::Entry { source, .. } => Some(source),
            Self::Version { .. } | Self::EmptyInstallDir | Self::SharedFolder { .. } => None,
        }
    }
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renamed { skill_name } => write!(
                f,
                "is installed under a new name (skillName {skill_name:?}); \
                 renamed skills cannot be installed because their SKILL.md differs from the source"
            ),
            Self::Repository(source) => source.fmt(f),
            Self::Commit(source) => source.fmt(f),
            Self::SkillPath(source) => source.fmt(f),
            Self::Hash { value } => write!(
                f,
                "has invalid \"computedHash\" {value:?} (expected a lowercase hex SHA-256 digest)"
            ),
        }
    }
}

impl Error for EntryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::SkillPath(source) => Some(source),
            Self::Renamed { .. } | Self::Hash { .. } => None,
        }
    }
}
