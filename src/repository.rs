//! The files of one repository at one commit.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

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
    Symlink(String),
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

    fn add_symlink(&mut self, path: &str, target: &str) -> Result<(), ArchiveError> {
        self.insert(path, Node::Symlink(target.to_owned()))
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
                let target = entry
                    .link_name_bytes()
                    .map(|bytes| bytes.into_owned())
                    .ok_or_else(|| ArchiveError::Unsupported {
                        path: inner.clone(),
                    })?;
                let target = std::str::from_utf8(&target).map_err(|_| ArchiveError::NotUtf8 {
                    lossy: String::from_utf8_lossy(&target).into_owned(),
                })?;
                repository.add_symlink(&inner, target)?;
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
    /// they are in the lock's `computedHash`. A symlink is replaced by the file it
    /// names when that file stays inside the skill folder. A link that is broken,
    /// loops, or leaves the folder is left out. An absolute link is an error.
    pub(crate) fn skill(
        &self,
        skill_path: &SkillPath,
        expected: &ContentHash,
    ) -> Result<Skill, SkillError> {
        let folder = skill_path.folder();
        if matches!(self.node(folder), Some(Node::Symlink(_))) {
            return Err(SkillError::FolderSymlink {
                path: folder.to_owned(),
            });
        }
        let mut files = BTreeMap::new();
        self.collect(folder, folder, "", &BTreeSet::new(), &mut files)?;
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

    fn node(&self, path: &str) -> Option<&Node> {
        self.nodes.get(&RepoPath::parse(path).ok()?)
    }

    fn children(&self, dir: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for path in self.nodes.keys() {
            let rest = if dir.is_empty() {
                path.as_str()
            } else {
                let Some(rest) = path.as_str().strip_prefix(dir) else {
                    continue;
                };
                let Some(rest) = rest.strip_prefix('/') else {
                    continue;
                };
                rest
            };
            if let Some(name) = rest.split('/').next()
                && !name.is_empty()
            {
                names.insert(name.to_owned());
            }
        }
        names
    }

    fn collect(
        &self,
        skill_root: &str,
        dir: &str,
        prefix: &str,
        ancestors: &BTreeSet<String>,
        files: &mut BTreeMap<String, SkillFile>,
    ) -> Result<(), SkillError> {
        if ancestors.contains(dir) {
            return Ok(());
        }
        let mut ancestors = ancestors.clone();
        ancestors.insert(dir.to_owned());
        for name in self.children(dir) {
            let path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let out = format!("{prefix}{name}");
            match self.node(&path) {
                Some(Node::File(file)) => {
                    if !Self::leave_out(&name, false) {
                        files.insert(out, file.clone());
                    }
                }
                Some(Node::Symlink(_)) => {
                    if Self::leave_out(&name, false) {
                        continue;
                    }
                    match self.follow(skill_root, &path, &mut BTreeSet::new())? {
                        Followed::Skip => {}
                        Followed::File(file) => {
                            files.insert(out, file);
                        }
                        Followed::Dir(target) if !Self::leave_out(&name, true) => {
                            self.collect(
                                skill_root,
                                &target,
                                &format!("{out}/"),
                                &ancestors,
                                files,
                            )?;
                        }
                        Followed::Dir(_) => {}
                    }
                }
                None if self.children(&path).is_empty() => {}
                None => {
                    if !Self::leave_out(&name, true) {
                        self.collect(skill_root, &path, &format!("{out}/"), &ancestors, files)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The file a symlink names, walking only paths inside `skill_root`.
    fn follow(
        &self,
        skill_root: &str,
        path: &str,
        chain: &mut BTreeSet<String>,
    ) -> Result<Followed, SkillError> {
        if !chain.insert(path.to_owned()) {
            return Ok(Followed::Skip);
        }
        let Some(Node::Symlink(target)) = self.node(path) else {
            return Ok(Followed::Skip);
        };
        let target = target.clone();
        if Path::new(&target).is_absolute() {
            return Err(SkillError::AbsoluteSymlink {
                path: Self::relative_to(skill_root, path).to_owned(),
            });
        }
        let Some(resolved) = Self::join_link(Self::parent(path), &target) else {
            return Ok(Followed::Skip);
        };
        if !Self::within(skill_root, &resolved) {
            return Ok(Followed::Skip);
        }
        match self.node(&resolved) {
            Some(Node::File(file)) => Ok(Followed::File(file.clone())),
            Some(Node::Symlink(_)) => self.follow(skill_root, &resolved, chain),
            None if !self.children(&resolved).is_empty() || resolved == skill_root => {
                Ok(Followed::Dir(resolved))
            }
            None => Ok(Followed::Skip),
        }
    }

    fn leave_out(name: &str, directory: bool) -> bool {
        name == "metadata.json" || (directory && EXCLUDED_FOLDERS.contains(&name))
    }

    fn parent(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(parent, _)| parent)
    }

    fn relative_to<'a>(root: &str, path: &'a str) -> &'a str {
        if root.is_empty() {
            path
        } else {
            path.strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(path)
        }
    }

    fn within(root: &str, path: &str) -> bool {
        root.is_empty() || path == root || path.starts_with(&format!("{root}/"))
    }

    /// `directory` joined with a relative link target. `None` when the target
    /// climbs above the repository root.
    fn join_link(directory: &str, target: &str) -> Option<String> {
        let mut parts: Vec<&str> = if directory.is_empty() {
            Vec::new()
        } else {
            directory.split('/').collect()
        };
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                other => parts.push(other),
            }
        }
        Some(parts.join("/"))
    }
}

enum Followed {
    File(SkillFile),
    Dir(String),
    Skip,
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
    /// `path` is relative to the skill folder. The link target is absolute.
    AbsoluteSymlink {
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
            Self::FolderSymlink { path } => write!(f, "is a symlink at \"{path}\""),
            Self::AbsoluteSymlink { path } => {
                write!(f, "contains an absolute symlink at \"{path}\"")
            }
            Self::HashMismatch { expected, actual } => write!(
                f,
                "does not match skills-lock.json: computedHash is {expected} but the fetched files hash to {actual}"
            ),
        }
    }
}

impl Error for SkillError {}
