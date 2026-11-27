//! The files of one repository at one commit.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read};

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::skill::{Skill, SkillFile};

const EXCLUDED_FOLDERS: [&str; 3] = [".git", "__pycache__", "__pypackages__"];

#[derive(Clone, Debug)]
enum Node {
    File(SkillFile),
    Symlink,
}

/// Every file and symlink in a repository, keyed by `/`-separated path.
#[derive(Clone, Debug, Default)]
pub struct Repository {
    nodes: BTreeMap<String, Node>,
}

impl Repository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, path: &str, file: SkillFile) {
        self.nodes.insert(path.to_owned(), Node::File(file));
    }

    /// Reads a gzipped tarball whose entries sit under one top-level folder, which
    /// is how GitHub lays out a commit. Symlinks are recorded and not resolved.
    pub fn from_tar_gz(reader: impl Read) -> Result<Self, ArchiveError> {
        let mut archive = tar::Archive::new(GzDecoder::new(reader));
        let mut repository = Self::new();
        let mut top_folder: Option<String> = None;
        for entry in archive.entries().map_err(ArchiveError::Read)? {
            let mut entry = entry.map_err(ArchiveError::Read)?;
            let entry_type = entry.header().entry_type();
            if entry_type.is_pax_global_extensions() {
                continue;
            }
            let bytes = entry.path_bytes();
            let path = std::str::from_utf8(&bytes)
                .map_err(|_| ArchiveError::invalid_path(String::from_utf8_lossy(&bytes)))?
                .trim_end_matches('/')
                .to_owned();
            if !plain_relative(&path) {
                return Err(ArchiveError::invalid_path(path));
            }
            let (top, inner) = path.split_once('/').unwrap_or((&path, ""));
            match &top_folder {
                None => top_folder = Some(top.to_owned()),
                Some(expected) if expected != top => {
                    return Err(ArchiveError::Invalid(format!(
                        "archive has more than one top-level folder ({expected:?} and {top:?})"
                    )));
                }
                Some(_) => {}
            }
            if entry_type.is_dir() || inner.is_empty() {
                continue;
            }
            if entry_type.is_symlink() {
                repository.nodes.insert(inner.to_owned(), Node::Symlink);
            } else if entry_type.is_file() || entry_type.is_contiguous() {
                let mode = entry.header().mode().map_err(ArchiveError::Read)?;
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(ArchiveError::Read)?;
                repository.nodes.insert(
                    inner.to_owned(),
                    Node::File(SkillFile {
                        contents,
                        executable: mode & 0o111 != 0,
                    }),
                );
            } else {
                return Err(ArchiveError::Invalid(format!(
                    "archive entry {inner:?} is not a file, folder, or symlink"
                )));
            }
        }
        Ok(repository)
    }

    /// The skill whose `SKILL.md` is at `skill_path`.
    ///
    /// `metadata.json`, `.git`, `__pycache__`, and `__pypackages__` are left out, as
    /// they are in the lock's `computedHash`. A symlink under the skill folder is an
    /// error and is not followed.
    pub(crate) fn skill(
        &self,
        skill_path: &str,
        expected: &ContentHash,
    ) -> Result<Skill, SkillError> {
        let (folder, file_name) = skill_path
            .rsplit_once('/')
            .map_or(("", skill_path), |(folder, name)| (folder, name));
        if !folder.is_empty() && matches!(self.nodes.get(folder), Some(Node::Symlink)) {
            return Err(SkillError::Symlink(String::new()));
        }

        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let mut files = BTreeMap::new();
        let mut symlink = None;
        for (path, node) in &self.nodes {
            let Some(relative) = path.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            match node {
                Node::Symlink => {
                    if symlink.is_none() {
                        symlink = Some(relative.to_owned());
                    }
                }
                Node::File(file) if !excluded(relative) => {
                    files.insert(relative.to_owned(), file.clone());
                }
                Node::File(_) => {}
            }
        }
        if let Some(path) = symlink {
            return Err(SkillError::Symlink(path));
        }
        if !files.contains_key(file_name) {
            return Err(SkillError::Missing);
        }
        Skill::from_files(files, expected).map_err(SkillError::HashMismatch)
    }

    /// License files at the repository root. A symlink is not a license file.
    pub fn license_files(&self) -> BTreeMap<String, SkillFile> {
        self.nodes
            .iter()
            .filter_map(|(path, node)| match node {
                Node::File(file) if !path.contains('/') && is_license_name(path) => {
                    Some((path.clone(), file.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains('\\')
                && !part.contains(':')
        })
}

fn excluded(relative: &str) -> bool {
    let mut parts = relative.split('/');
    let name = parts.next_back().unwrap_or(relative);
    name == "metadata.json"
        || parts.any(|part| part == "metadata.json" || EXCLUDED_FOLDERS.contains(&part))
}

fn is_license_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let (stem, extension) = upper
        .split_once('.')
        .map_or((upper.as_str(), None), |(stem, extension)| {
            (stem, Some(extension))
        });
    matches!(stem, "LICENSE" | "LICENCE" | "COPYING" | "UNLICENSE")
        || (stem.starts_with("LICENSE-") || stem.starts_with("LICENCE-"))
            && matches!(extension, None | Some("MD" | "TXT"))
}

/// Why a source archive could not be read.
#[derive(Debug)]
pub enum ArchiveError {
    Read(io::Error),
    Invalid(String),
}

impl ArchiveError {
    fn invalid_path(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::Invalid(format!(
            "archive path {path:?} must be relative, with no empty, \".\", or \"..\" \
             component, and no '\\' or ':'"
        ))
    }
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(source) => write!(f, "cannot read archive: {source}"),
            Self::Invalid(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug)]
pub(crate) enum SkillError {
    Missing,
    /// Empty when the skill folder itself is a symlink.
    Symlink(String),
    HashMismatch(ContentHash),
}
