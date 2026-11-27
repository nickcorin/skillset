//! A skill is the files it installs. One exists only for content whose hash is the
//! digest that was asked for.

use std::collections::BTreeMap;
use std::fmt;

use crate::content_hash::ContentHash;

/// One file's bytes, and whether the repository marks it executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    pub contents: Vec<u8>,
    pub executable: bool,
}

impl SkillFile {
    pub fn new(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
            executable: false,
        }
    }

    pub fn executable(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
            executable: true,
        }
    }
}

/// The files of one skill, identified by their [`ContentHash`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    hash: ContentHash,
    files: BTreeMap<String, SkillFile>,
}

impl Skill {
    pub(crate) fn from_files(
        files: BTreeMap<String, SkillFile>,
        expected: &ContentHash,
    ) -> Result<Self, ContentHash> {
        let hash = ContentHash::of_files(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.contents.as_slice())),
        );
        if &hash == expected {
            Ok(Self { hash, files })
        } else {
            Err(hash)
        }
    }

    pub(crate) fn files(&self) -> &BTreeMap<String, SkillFile> {
        &self.files
    }
}

/// Repository path of a skill's `SKILL.md`, such as `skills/pdf/SKILL.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillPath(String);

impl SkillPath {
    pub(crate) fn parse(value: &str) -> Result<Self, SkillPathError> {
        let names_skill = value
            .rsplit('/')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"));
        let plain = !value.is_empty()
            && value.split('/').all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.contains('\\')
            });
        if names_skill && plain {
            Ok(Self(value.to_owned()))
        } else {
            Err(SkillPathError {
                value: value.to_owned(),
            })
        }
    }

    /// Folder of the skill inside the repository. Empty when `SKILL.md` is at the root.
    pub(crate) fn folder(&self) -> &str {
        self.0.rsplit_once('/').map_or("", |(folder, _)| folder)
    }

    pub(crate) fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.0.as_str(), |(_, name)| name)
    }
}

impl fmt::Display for SkillPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `skillPath` was not a relative path to `SKILL.md`.
#[derive(Debug)]
pub struct SkillPathError {
    value: String,
}

impl fmt::Display for SkillPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "has invalid \"skillPath\" {:?} \
             (expected a relative path to SKILL.md such as skills/<name>/SKILL.md)",
            self.value
        )
    }
}

impl std::error::Error for SkillPathError {}
