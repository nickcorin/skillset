//! The files of one repository at one commit.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::skill::{Skill, SkillFile, SkillPath};

/// `fs.realpath` gives up after this many symlink hops. One more is `ELOOP`.
const SYMLINK_HOP_LIMIT: u32 = 40;
const EXCLUDED_FOLDERS: [&str; 3] = [".git", "__pycache__", "__pypackages__"];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RepoPath(String);

enum Relation<'a> {
    Same,
    Inside(&'a str),
}

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

    fn from_component(component: &str) -> Option<Self> {
        Self::parse(component).ok()
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    fn parent(&self) -> Option<Self> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_owned()))
    }

    fn join(&self, component: &str) -> Option<Self> {
        let child = Self::from_component(component)?;
        Some(Self(format!("{}/{}", self.0, child.as_str())))
    }

    fn relative_to<'a>(&'a self, root: &Self) -> Option<Relation<'a>> {
        if self == root {
            Some(Relation::Same)
        } else {
            self.0
                .strip_prefix(root.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .map(Relation::Inside)
        }
    }

    fn within(&self, root: &Self) -> bool {
        match self.relative_to(root) {
            Some(Relation::Same) => true,
            Some(Relation::Inside(rest)) => !rest.is_empty(),
            None => false,
        }
    }
}

/// A location in the repository. The root has no path of its own.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Root,
    Path(RepoPath),
}

impl Place {
    fn path(&self) -> Option<&RepoPath> {
        match self {
            Self::Root => None,
            Self::Path(path) => Some(path),
        }
    }

    fn join(&self, component: &str) -> Option<Self> {
        let path = match self {
            Self::Root => RepoPath::from_component(component)?,
            Self::Path(path) => path.join(component)?,
        };
        Some(Self::Path(path))
    }

    fn contains(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Root, _) => true,
            (Self::Path(root), Self::Path(path)) => path.within(root),
            (Self::Path(_), Self::Root) => false,
        }
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

    pub fn add_symlink(&mut self, path: &str, target: &str) -> Result<(), ArchiveError> {
        self.insert(path, Node::Symlink(target.to_owned()))
    }

    fn insert(&mut self, path: &str, node: Node) -> Result<(), ArchiveError> {
        self.nodes.insert(RepoPath::parse(path)?, node);
        Ok(())
    }

    /// Reads a gzipped tarball whose entries sit under one top-level folder, which
    /// is how GitHub lays out a commit. Symlink targets are stored and resolved
    /// when a skill is read.
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
    /// they are in the lock's `computedHash`. Each path is resolved one component at
    /// a time, the way `realpath` does. A symlink becomes the file or folder it names
    /// when that target stays inside the skill folder. A link that is broken, loops,
    /// leaves the folder, or is absolute is left out.
    pub(crate) fn skill(
        &self,
        skill_path: &SkillPath,
        expected: &ContentHash,
    ) -> Result<Skill, SkillError> {
        let folder = match skill_path.folder() {
            Some(path) => Some(RepoPath::parse(path).map_err(|_| SkillError::Missing {
                path: skill_path.clone(),
            })?),
            None => None,
        };
        let Some(root) = self.canonicalize(folder.as_ref()) else {
            return Err(SkillError::Missing {
                path: skill_path.clone(),
            });
        };
        if !self.is_directory(&root) {
            return Err(SkillError::Missing {
                path: skill_path.clone(),
            });
        }
        let mut files = BTreeMap::new();
        self.collect(&root, &root, None, &BTreeSet::new(), &mut files);
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

    fn canonicalize(&self, path: Option<&RepoPath>) -> Option<Place> {
        let parts = path
            .map(|path| path.components().map(str::to_owned).collect())
            .unwrap_or_default();
        self.walk(None, parts, &mut 0)
    }

    /// `cursor` is the directory already resolved. `None` is the repository root.
    fn walk(
        &self,
        mut cursor: Option<RepoPath>,
        mut parts: Vec<String>,
        hops: &mut u32,
    ) -> Option<Place> {
        let mut index = 0;
        while index < parts.len() {
            let component = parts[index].clone();
            index += 1;
            match component.as_str() {
                "" | "." => {}
                ".." => {
                    let path = cursor?;
                    cursor = path.parent();
                }
                name => {
                    let next = match &cursor {
                        Some(dir) => dir.join(name)?,
                        None => RepoPath::from_component(name)?,
                    };
                    match self.nodes.get(&next) {
                        Some(Node::Symlink(target)) => {
                            *hops += 1;
                            if *hops > SYMLINK_HOP_LIMIT {
                                return None;
                            }
                            if Path::new(target).is_absolute() {
                                return None;
                            }
                            let mut restarted: Vec<String> =
                                target.split('/').map(str::to_owned).collect();
                            restarted.extend(parts.iter().skip(index).cloned());
                            parts = restarted;
                            index = 0;
                        }
                        Some(Node::File(_)) => {
                            if index != parts.len() {
                                return None;
                            }
                            return Some(Place::Path(next));
                        }
                        None => {
                            if !self.is_dir(&next) {
                                return None;
                            }
                            cursor = Some(next);
                        }
                    }
                }
            }
        }
        Some(match cursor {
            Some(path) => Place::Path(path),
            None => Place::Root,
        })
    }

    fn is_directory(&self, place: &Place) -> bool {
        match place {
            Place::Root => true,
            Place::Path(path) => self.is_dir(path),
        }
    }

    fn is_dir(&self, path: &RepoPath) -> bool {
        let prefix = format!("{}/", path.as_str());
        self.nodes
            .keys()
            .any(|key| key.as_str().starts_with(&prefix))
    }

    fn child_names(&self, place: &Place) -> BTreeSet<String> {
        let prefix = place.path().map(|path| format!("{}/", path.as_str()));
        let mut names = BTreeSet::new();
        for path in self.nodes.keys() {
            let rest = match &prefix {
                None => path.as_str(),
                Some(prefix) => {
                    let Some(rest) = path.as_str().strip_prefix(prefix.as_str()) else {
                        continue;
                    };
                    rest
                }
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
        logical: &Place,
        root: &Place,
        prefix: Option<&str>,
        ancestors: &BTreeSet<Place>,
        files: &mut BTreeMap<String, SkillFile>,
    ) {
        let Some(real) = self.canonicalize(logical.path()) else {
            return;
        };
        if !self.is_directory(&real) || ancestors.contains(&real) {
            return;
        }
        let mut branch = ancestors.clone();
        branch.insert(real.clone());
        for name in self.child_names(&real) {
            let Some(Place::Path(at)) = real.join(&name) else {
                continue;
            };
            let Some(logical_child) = logical.join(&name) else {
                continue;
            };
            let key = match prefix {
                Some(prefix) => format!("{prefix}/{name}"),
                None => name.clone(),
            };
            match self.nodes.get(&at) {
                Some(Node::File(file)) => {
                    if name != "metadata.json" {
                        files.insert(key, file.clone());
                    }
                }
                Some(Node::Symlink(_)) => {
                    if name == "metadata.json" {
                        continue;
                    }
                    let Some(resolved) = self.canonicalize(logical_child.path()) else {
                        continue;
                    };
                    if !root.contains(&resolved) {
                        continue;
                    }
                    if self.is_directory(&resolved) {
                        if EXCLUDED_FOLDERS.contains(&name.as_str()) {
                            continue;
                        }
                        self.collect(&logical_child, root, Some(&key), &branch, files);
                    } else if let Place::Path(path) = &resolved
                        && let Some(Node::File(file)) = self.nodes.get(path)
                    {
                        files.insert(key, file.clone());
                    }
                }
                None if self.is_dir(&at) => {
                    if name == "metadata.json" || EXCLUDED_FOLDERS.contains(&name.as_str()) {
                        continue;
                    }
                    self.collect(&logical_child, root, Some(&key), &branch, files);
                }
                None => {}
            }
        }
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
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "not found at {path}"),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "does not match skills-lock.json: computedHash is {expected} but the fetched files hash to {actual}"
            ),
        }
    }
}

impl Error for SkillError {}
