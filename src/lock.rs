//! A version 2 `skills-lock.json`: GitHub entries, plus an optional `skillset.installDir`
//! that `npx skills` does not read.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::content_hash::ContentHash;

/// A parsed lock file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lockfile {
    /// `skillset.installDir`, relative to the lock file's folder.
    pub install_dir: Option<PathBuf>,
    /// Entries in skill-name order.
    pub entries: Vec<LockEntry>,
}

/// One GitHub skill at a pinned commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    /// Folder name under the install directory.
    pub folder: String,
    pub owner: String,
    pub repo: String,
    pub commit: String,
    /// Repository path of `SKILL.md`, such as `skills/pdf/SKILL.md`.
    pub skill_path: String,
    pub hash: ContentHash,
}

impl LockEntry {
    pub fn source(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Why a lock file could not be read.
#[derive(Debug)]
pub enum LockError {
    Read { path: PathBuf, source: io::Error },
    Invalid { path: PathBuf, detail: String },
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self, LockError> {
        let json = fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&json).map_err(|detail| LockError::Invalid {
            path: path.to_owned(),
            detail,
        })
    }

    fn from_json(json: &str) -> Result<Self, String> {
        let raw: RawLock = serde_json::from_str(json)
            .map_err(|err| format!("not a valid skills lock file: {err}"))?;
        if raw.version != 2 {
            return Err(format!(
                "unsupported lock version {} (expected 2)",
                raw.version
            ));
        }
        let install_dir = match raw.skillset {
            Some(settings) if settings.install_dir.is_empty() => {
                return Err("\"skillset.installDir\" is empty".to_owned());
            }
            Some(settings) => Some(PathBuf::from(settings.install_dir)),
            None => None,
        };

        let mut entries = Vec::with_capacity(raw.skills.len());
        let mut folders = BTreeSet::new();
        for (name, entry) in raw.skills {
            let entry = LockEntry::from_raw(name, entry)?;
            if !folders.insert(entry.folder.clone()) {
                return Err(format!(
                    "more than one skill would install into folder \"{}\"",
                    entry.folder
                ));
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
    fn from_raw(name: String, raw: RawEntry) -> Result<Self, String> {
        if raw.source_type != "github" {
            return Err(format!(
                "skill \"{name}\" has sourceType {:?}; only \"github\" sources can be installed",
                raw.source_type
            ));
        }
        if let Some(source_name) = raw.skill_name {
            return Err(format!(
                "skill \"{name}\" is installed under a new name (skillName {source_name:?}); \
                 renamed skills cannot be installed because their SKILL.md differs from the source"
            ));
        }
        let Some((owner, repo)) = github_source(&raw.source) else {
            return Err(format!(
                "skill \"{name}\" has invalid \"source\" {:?} (expected owner/repo)",
                raw.source
            ));
        };
        if raw.commit.len() != 40 || !raw.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "skill \"{name}\" has invalid \"commit\" {:?} (expected a full 40-character hex commit SHA)",
                raw.commit
            ));
        }
        if !is_skill_path(&raw.skill_path) {
            return Err(format!(
                "skill \"{name}\" has invalid \"skillPath\" {:?} \
                 (expected a relative path to SKILL.md such as skills/<name>/SKILL.md)",
                raw.skill_path
            ));
        }
        let Some(hash) = ContentHash::parse(&raw.computed_hash) else {
            return Err(format!(
                "skill \"{name}\" has invalid \"computedHash\" {:?} (expected a lowercase hex SHA-256 digest)",
                raw.computed_hash
            ));
        };
        Ok(Self {
            folder: folder_name(&name),
            name,
            owner,
            repo,
            commit: raw.commit,
            skill_path: raw.skill_path,
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
    source_type: String,
    commit: String,
    skill_path: String,
    computed_hash: String,
    #[serde(default)]
    skill_name: Option<String>,
}

/// `owner/repo` as `npx skills` accepts it, with an optional `.git` suffix.
fn github_source(source: &str) -> Option<(String, String)> {
    let (owner, repo) = source.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let owner_ok = !owner.is_empty()
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let repo_ok = !repo.is_empty()
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    (owner_ok && repo_ok).then(|| (owner.to_owned(), repo.to_owned()))
}

fn is_skill_path(path: &str) -> bool {
    let names_skill = path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"));
    names_skill
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
}

/// The install folder name `npx skills` derives in `sanitizeName`.
fn folder_name(name: &str) -> String {
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
    if truncated.is_empty() {
        "unnamed-skill".to_owned()
    } else {
        truncated.to_owned()
    }
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Invalid { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}
