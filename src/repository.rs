//! The files of one repository at one commit.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::skill::{Skill, SkillFile, SkillPath};

const EXCLUDED_FOLDERS: [&str; 3] = [".git", "__pycache__", "__pypackages__"];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RepoPath(String);

impl RepoPath {
    fn parse(path: &str) -> Result<Self, RepoPathError> {
        let plain = !path.is_empty()
            && path.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.contains('\\')
                    && !part.contains(':')
            });
        if plain {
            Ok(Self(path.to_owned()))
        } else {
            Err(RepoPathError {
                path: path.to_owned(),
            })
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn is_license_file(&self) -> bool {
        let Some(name) = self.0.rsplit('/').next() else {
            return false;
        };
        if self.0.contains('/') {
            return false;
        }
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

    /// `metadata.json`, `.git`, `__pycache__`, and `__pypackages__`, relative to a skill folder.
    fn excluded_from_hash(&self) -> bool {
        let mut parts = self.0.split('/');
        let name = parts.next_back().unwrap_or(self.0.as_str());
        name == "metadata.json"
            || parts.any(|part| part == "metadata.json" || EXCLUDED_FOLDERS.contains(&part))
    }
}

/// A repository path with an empty, `.`, `..`, `\`, or `:` component.
#[derive(Debug)]
pub struct RepoPathError {
    path: String,
}

impl fmt::Display for RepoPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "archive path {:?} must be relative, with no empty, \".\", or \"..\" \
             component, and no '\\' or ':'",
            self.path
        )
    }
}

impl Error for RepoPathError {}

#[derive(Clone, Debug)]
enum Node {
    File(SkillFile),
    Symlink,
}

/// Every file and symlink in a repository, keyed by `/`-separated path.
#[derive(Clone, Debug, Default)]
pub struct Repository {
    nodes: BTreeMap<RepoPath, Node>,
}

impl Repository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, path: &str, file: SkillFile) -> Result<(), ArchiveError> {
        self.insert(path, Node::File(file))
    }

    fn add_symlink(&mut self, path: &str) -> Result<(), ArchiveError> {
        self.insert(path, Node::Symlink)
    }

    fn insert(&mut self, path: &str, node: Node) -> Result<(), ArchiveError> {
        self.nodes.insert(RepoPath::parse(path)?, node);
        Ok(())
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
                .map_err(|_| ArchiveError::NotUtf8 {
                    lossy: String::from_utf8_lossy(&bytes).into_owned(),
                })?
                .trim_end_matches('/')
                .to_owned();
            RepoPath::parse(&path)?;
            let (top, inner) = path.split_once('/').unwrap_or((&path, ""));
            let inner = inner.to_owned();
            match &top_folder {
                None => top_folder = Some(top.to_owned()),
                Some(expected) if expected != top => {
                    return Err(ArchiveError::MultipleFolders {
                        first: expected.clone(),
                        second: top.to_owned(),
                    });
                }
                Some(_) => {}
            }
            if entry_type.is_dir() || inner.is_empty() {
                continue;
            }
            if entry_type.is_symlink() {
                repository.add_symlink(&inner)?;
            } else if entry_type.is_file() || entry_type.is_contiguous() {
                let mode = entry.header().mode().map_err(ArchiveError::Read)?;
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(ArchiveError::Read)?;
                repository.add_file(
                    &inner,
                    SkillFile {
                        contents,
                        executable: mode & 0o111 != 0,
                    },
                )?;
            } else {
                return Err(ArchiveError::Unsupported { path: inner });
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
        skill_path: &SkillPath,
        expected: &ContentHash,
    ) -> Result<Skill, SkillError> {
        let folder = skill_path.folder();
        if let Ok(folder_path) = RepoPath::parse(folder)
            && matches!(self.nodes.get(&folder_path), Some(Node::Symlink))
        {
            return Err(SkillError::FolderSymlink {
                path: folder_path.as_str().to_owned(),
            });
        }

        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let mut files = BTreeMap::new();
        let mut symlink = None;
        for (path, node) in &self.nodes {
            let Some(relative) = path.as_str().strip_prefix(&prefix) else {
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
                Node::File(file) => {
                    let include = match RepoPath::parse(relative) {
                        Ok(relative_path) => !relative_path.excluded_from_hash(),
                        Err(_) => true,
                    };
                    if include {
                        files.insert(relative.to_owned(), file.clone());
                    }
                }
            }
        }
        if let Some(path) = symlink {
            return Err(SkillError::ContainsSymlink { path });
        }
        if !files.contains_key(skill_path.file_name()) {
            return Err(SkillError::Missing {
                path: skill_path.clone(),
            });
        }
        Skill::from_files(files, expected).map_err(|actual| SkillError::HashMismatch {
            expected: expected.clone(),
            actual,
        })
    }

    /// License files at the repository root. A symlink is not a license file.
    pub fn license_files(&self) -> BTreeMap<String, SkillFile> {
        self.nodes
            .iter()
            .filter_map(|(path, node)| match node {
                Node::File(file) if path.is_license_file() => {
                    Some((path.as_str().to_owned(), file.clone()))
                }
                Node::File(_) | Node::Symlink => None,
            })
            .collect()
    }
}

/// Why a source archive could not be read.
#[derive(Debug)]
pub enum ArchiveError {
    Read(io::Error),
    Path(RepoPathError),
    NotUtf8 { lossy: String },
    MultipleFolders { first: String, second: String },
    Unsupported { path: String },
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(source) => write!(f, "cannot read archive: {source}"),
            Self::Path(source) => source.fmt(f),
            Self::NotUtf8 { lossy } => write!(f, "archive path {lossy:?} is not UTF-8"),
            Self::MultipleFolders { first, second } => write!(
                f,
                "archive has more than one top-level folder ({first:?} and {second:?})"
            ),
            Self::Unsupported { path } => {
                write!(
                    f,
                    "archive entry {path:?} is not a file, folder, or symlink"
                )
            }
        }
    }
}

impl Error for ArchiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(source) => Some(source),
            Self::Path(source) => Some(source),
            Self::NotUtf8 { .. } | Self::MultipleFolders { .. } | Self::Unsupported { .. } => None,
        }
    }
}

impl From<RepoPathError> for ArchiveError {
    fn from(source: RepoPathError) -> Self {
        Self::Path(source)
    }
}

#[derive(Debug)]
pub enum SkillError {
    Missing {
        path: SkillPath,
    },
    /// `path` is the skill folder. It is never empty.
    FolderSymlink {
        path: String,
    },
    /// `path` is relative to the skill folder. It is never empty.
    ContainsSymlink {
        path: String,
    },
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "not found at {path}"),
            Self::FolderSymlink { .. } => f.write_str("is a symlink"),
            Self::ContainsSymlink { path } => write!(f, "contains a symlink at \"{path}\""),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "does not match skills-lock.json: computedHash is {expected} but the fetched files hash to {actual}"
            ),
        }
    }
}

impl Error for SkillError {}
