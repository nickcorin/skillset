//! A skill is the files it installs. One exists only for content whose hash is the
//! digest that was asked for.

use std::collections::BTreeMap;

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

    pub(crate) fn hash(&self) -> &ContentHash {
        &self.hash
    }

    pub(crate) fn files(&self) -> &BTreeMap<String, SkillFile> {
        &self.files
    }
}
