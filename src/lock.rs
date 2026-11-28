//! A version 2 `skills-lock.json`: the skills to restore, and an optional install folder.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::content_hash::ContentHash;
use crate::error::Error;

/// The lock file install restores from.
pub struct Lock {
    install_dir: Option<PathBuf>,
    entries: Vec<Entry>,
}

/// One skill recorded in the lock: where it lives on GitHub, and the hash it must match.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) commit: String,
    pub(crate) skill_path: String,
    pub(crate) hash: ContentHash,
}

impl Lock {
    pub fn read(path: &Path) -> Result<Self, Error> {
        let json = fs::read_to_string(path).map_err(|source| Error::io("read", path, source))?;
        let raw: RawLock = serde_json::from_str(&json).map_err(|source| {
            Error::new(format!(
                "{}: not a valid skills lock file: {source}",
                path.display()
            ))
        })?;
        if raw.version != 2 {
            return Err(Error::new(format!(
                "{}: unsupported lock version {} (expected 2)",
                path.display(),
                raw.version
            )));
        }
        let install_dir = match raw.skillset {
            Some(settings) if settings.install_dir.is_empty() => {
                return Err(Error::new(format!(
                    "{}: \"skillset.installDir\" is empty",
                    path.display()
                )));
            }
            Some(settings) => Some(PathBuf::from(settings.install_dir)),
            None => None,
        };
        let mut entries = Vec::with_capacity(raw.skills.len());
        for (name, entry) in raw.skills {
            entries.push(Entry::parse(path, name, entry)?);
        }
        Ok(Self {
            install_dir,
            entries,
        })
    }

    pub(crate) fn install_dir(&self) -> Option<&Path> {
        self.install_dir.as_deref()
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

impl Entry {
    fn parse(lock_path: &Path, name: String, raw: RawEntry) -> Result<Self, Error> {
        fn github_source(source: &str) -> Option<String> {
            let trimmed = source.strip_suffix(".git").unwrap_or(source);
            let (owner, name) = trimmed.split_once('/')?;
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
            (owner_ok && name_ok).then(|| format!("{owner}/{name}"))
        }

        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            return Err(Error::new(format!(
                "{}: skill {name:?} cannot be installed as a folder",
                lock_path.display()
            )));
        }
        if raw.source_type != "github" {
            return Err(Error::new(format!(
                "{}: skill {name:?} has sourceType {:?} (expected \"github\")",
                lock_path.display(),
                raw.source_type
            )));
        }
        let Some(source) = github_source(&raw.source) else {
            return Err(Error::new(format!(
                "{}: skill {name:?} has invalid \"source\" {:?} (expected owner/repo)",
                lock_path.display(),
                raw.source
            )));
        };
        if raw.commit.len() != 40 || !raw.commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::new(format!(
                "{}: skill {name:?} has invalid \"commit\" {:?} \
                 (expected a full 40-character hex commit SHA)",
                lock_path.display(),
                raw.commit
            )));
        }
        let names_skill = raw
            .skill_path
            .rsplit('/')
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case("SKILL.md"));
        let plain_path = !raw.skill_path.is_empty()
            && raw.skill_path.split('/').all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
            });
        if !names_skill || !plain_path {
            return Err(Error::new(format!(
                "{}: skill {name:?} has invalid \"skillPath\" {:?} \
                 (expected a relative path to SKILL.md)",
                lock_path.display(),
                raw.skill_path
            )));
        }
        let Some(hash) = ContentHash::parse(&raw.computed_hash) else {
            return Err(Error::new(format!(
                "{}: skill {name:?} has invalid \"computedHash\" {:?} \
                 (expected a lowercase hex SHA-256 digest)",
                lock_path.display(),
                raw.computed_hash
            )));
        };
        Ok(Self {
            name,
            source,
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
#[serde(rename_all = "camelCase")]
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
}
