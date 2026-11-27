//! The install folder.
//!
//! `<skill>/` holds that skill's files. `.licenses/<skill>/` holds the license
//! files from the root of its repository. Every other entry is removed. A symlink
//! is removed as itself and is never followed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::content_hash::ContentHash;
use crate::lock::{CommitSha, FolderName, GitHubRepo, LockEntry, LockError, Lockfile, SkillPath};
use crate::repository::{Repository, SkillError};
use crate::skill::{Skill, SkillFile};
use crate::source::Source;

const DEFAULT_INSTALL_DIR: &str = ".agents/skills";
const LICENSES_FOLDER: &str = ".licenses";

/// The folder a lock is restored into. It is always strictly inside the lock file's
/// folder.
pub struct InstallFolder {
    /// The lock file's folder. Ancestors of this are left alone.
    base: PathBuf,
    /// Path of the install folder relative to `base`.
    relative: PathBuf,
}

struct LockedSkill {
    folder: FolderName,
    skill: Skill,
    licenses: BTreeMap<String, SkillFile>,
}

enum NodeKind {
    Missing,
    Symlink,
    Directory,
    Other,
}

impl InstallFolder {
    /// Restores every skill in the lock at `lock_path` into this folder.
    ///
    /// `dir` and the lock's `skillset.installDir` are relative to the lock file's
    /// folder. When neither is set, skills install into `.agents/skills` there.
    /// Every skill is read and checked before the folder changes.
    pub fn install<S: Source>(
        lock_path: &Path,
        dir: Option<&Path>,
        source: &S,
    ) -> Result<(), InstallError> {
        let lock = Lockfile::read(lock_path)?;
        let folder = Self::at(lock_path, dir, lock.install_dir.as_deref())?;
        let skills = LockedSkill::load(&lock, source)?;
        folder.apply(&skills)
    }

    fn at(
        lock_path: &Path,
        flag: Option<&Path>,
        from_lock: Option<&Path>,
    ) -> Result<Self, InstallError> {
        let (raw, which) = if let Some(path) = flag {
            (path, "--dir")
        } else if let Some(path) = from_lock {
            (path, "skillset.installDir")
        } else {
            (Path::new(DEFAULT_INSTALL_DIR), "skillset.installDir")
        };
        let relative = Self::normalize(raw).map_err(|problem| InstallError::Directory {
            which,
            path: raw.to_path_buf(),
            problem,
        })?;
        Ok(Self {
            base: lock_path.parent().unwrap_or(Path::new("")).to_path_buf(),
            relative,
        })
    }

    /// A relative path that still names a folder inside its base after `.` and `..`
    /// are collapsed. The base itself is not inside itself.
    fn normalize(path: &Path) -> Result<PathBuf, DirectoryProblem> {
        if path.is_absolute() {
            return Err(DirectoryProblem::Absolute);
        }
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_os_string()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if parts.pop().is_none() {
                        return Err(DirectoryProblem::Outside);
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(DirectoryProblem::Absolute);
                }
            }
        }
        if parts.is_empty() {
            return Err(DirectoryProblem::Outside);
        }
        let mut relative = PathBuf::new();
        for part in parts {
            relative.push(part);
        }
        Ok(relative)
    }

    fn full_path(&self) -> PathBuf {
        self.base.join(&self.relative)
    }

    fn apply(&self, skills: &[LockedSkill]) -> Result<(), InstallError> {
        if !skills.is_empty() {
            self.create_root()
                .map_err(|source| InstallError::io("create", &self.full_path(), source))?;
            for item in skills {
                let skill_path = self.full_path().join(item.folder.as_str());
                self.place(&skill_path, item.skill.files(), item.skill.hash())?;
                let license_path = self
                    .full_path()
                    .join(LICENSES_FOLDER)
                    .join(item.folder.as_str());
                let license_hash = ContentHash::of_files(
                    item.licenses
                        .iter()
                        .map(|(path, file)| (path.as_str(), file.contents.as_slice())),
                );
                self.place(&license_path, &item.licenses, &license_hash)?;
            }
        }
        self.remove_except(&skills.iter().map(|item| item.folder.clone()).collect())
    }

    /// Creates the install folder as a real directory. A symlink in the relative
    /// path is removed and replaced; it is not followed. `base` is only created,
    /// never replaced.
    fn create_root(&self) -> io::Result<()> {
        if !self.base.as_os_str().is_empty() {
            fs::create_dir_all(&self.base)?;
        }
        let mut prefix = self.base.clone();
        for component in self.relative.components() {
            prefix.push(component);
            match kind(&prefix)? {
                NodeKind::Directory => {}
                NodeKind::Missing => fs::create_dir(&prefix)?,
                NodeKind::Symlink | NodeKind::Other => {
                    remove_leaf(&prefix)?;
                    fs::create_dir(&prefix)?;
                }
            }
        }
        Ok(())
    }

    fn place(
        &self,
        path: &Path,
        files: &BTreeMap<String, SkillFile>,
        expected: &ContentHash,
    ) -> Result<(), InstallError> {
        let unchanged = !self
            .symlink_blocks(path)
            .map_err(|source| InstallError::io("inspect", path, source))?
            && match directory_hash(path) {
                Ok(hash) => hash.as_ref() == Some(expected),
                Err(source) => return Err(InstallError::io("read", path, source)),
            };
        if unchanged {
            return Ok(());
        }
        self.remove_path(path)
            .map_err(|source| InstallError::io("remove", path, source))?;
        self.write_files(path, files)
            .map_err(|source| InstallError::io("write", path, source))
    }

    fn remove_except(&self, kept: &BTreeSet<FolderName>) -> Result<(), InstallError> {
        let Some(root) = self
            .real_root()
            .map_err(|source| InstallError::io("inspect", &self.full_path(), source))?
        else {
            return Ok(());
        };
        let entries = fs::read_dir(&root)
            .and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
            .map_err(|source| InstallError::io("read", &root, source))?;
        for entry in entries {
            let path = entry.path();
            let result = if entry.file_name() == OsStr::new(LICENSES_FOLDER) {
                self.retain_licenses(&path, kept)
            } else if is_kept(&entry.file_name(), kept) {
                Ok(())
            } else {
                self.remove_path(&path)
            };
            result.map_err(|source| InstallError::io("remove", &path, source))?;
        }
        Ok(())
    }

    /// The install folder when every component below `base` is a real directory.
    /// A symlink in that relative path is removed, and its target is not opened.
    /// `base` is the lock file's own folder, so a symlink there is the folder the
    /// lock was opened through and is not replaced.
    fn real_root(&self) -> io::Result<Option<PathBuf>> {
        if !self.base.as_os_str().is_empty() {
            match kind(&self.base)? {
                NodeKind::Directory | NodeKind::Symlink => {}
                NodeKind::Missing => return Ok(None),
                NodeKind::Other => {
                    return Err(io::Error::other(format!(
                        "{} is not a directory",
                        self.base.display()
                    )));
                }
            }
        }
        let mut prefix = self.base.clone();
        for component in self.relative.components() {
            prefix.push(component);
            match kind(&prefix)? {
                NodeKind::Directory => {}
                NodeKind::Missing => return Ok(None),
                NodeKind::Symlink | NodeKind::Other => {
                    remove_leaf(&prefix)?;
                    return Ok(None);
                }
            }
        }
        Ok(Some(prefix))
    }

    /// Whether a symlink sits strictly between the install folder and `path`.
    fn symlink_blocks(&self, path: &Path) -> io::Result<bool> {
        let root = self.full_path();
        let Ok(relative) = path.strip_prefix(&root) else {
            return Ok(true);
        };
        let mut prefix = root;
        let components: Vec<_> = relative.components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            prefix.push(component);
            match kind(&prefix)? {
                NodeKind::Symlink => return Ok(true),
                NodeKind::Missing => return Ok(false),
                NodeKind::Directory | NodeKind::Other => {}
            }
        }
        Ok(false)
    }

    fn remove_path(&self, path: &Path) -> io::Result<()> {
        let root = self.full_path();
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("{} is outside the install folder", path.display()))
        })?;
        let mut prefix = root;
        let components: Vec<_> = relative.components().collect();
        for (index, component) in components.iter().enumerate() {
            prefix.push(component);
            let is_last = index + 1 == components.len();
            match kind(&prefix)? {
                NodeKind::Symlink => return remove_leaf(&prefix),
                NodeKind::Directory if is_last => return remove_tree(&prefix),
                NodeKind::Other if is_last => return remove_leaf(&prefix),
                NodeKind::Directory | NodeKind::Other => {}
                NodeKind::Missing => return Ok(()),
            }
        }
        Ok(())
    }

    fn write_files(&self, directory: &Path, files: &BTreeMap<String, SkillFile>) -> io::Result<()> {
        self.ensure_directory(directory)?;
        for (relative, file) in files {
            let mut target = directory.to_path_buf();
            for part in relative.split('/') {
                target.push(part);
            }
            if let Some(parent) = target.parent() {
                self.ensure_directory(parent)?;
            }
            fs::write(&target, &file.contents)?;
            mark_executable(&target, file.executable)?;
        }
        Ok(())
    }

    /// Creates `path` as a real directory below the install folder, replacing a
    /// symlink or file at any component without following it.
    fn ensure_directory(&self, path: &Path) -> io::Result<()> {
        let root = self.full_path();
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("{} is outside the install folder", path.display()))
        })?;
        let mut prefix = root;
        match kind(&prefix)? {
            NodeKind::Directory => {}
            NodeKind::Missing => fs::create_dir(&prefix)?,
            NodeKind::Symlink | NodeKind::Other => {
                remove_leaf(&prefix)?;
                fs::create_dir(&prefix)?;
            }
        }
        for component in relative.components() {
            prefix.push(component);
            match kind(&prefix)? {
                NodeKind::Directory => {}
                NodeKind::Missing => fs::create_dir(&prefix)?,
                NodeKind::Symlink | NodeKind::Other => {
                    remove_leaf(&prefix)?;
                    fs::create_dir(&prefix)?;
                }
            }
        }
        Ok(())
    }

    fn retain_licenses(&self, path: &Path, kept: &BTreeSet<FolderName>) -> io::Result<()> {
        match kind(path)? {
            NodeKind::Directory => {}
            NodeKind::Missing => return Ok(()),
            NodeKind::Symlink | NodeKind::Other => return remove_leaf(path),
        }
        for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
            if !is_kept(&entry.file_name(), kept) {
                self.remove_path(&entry.path())?;
            }
        }
        if fs::read_dir(path)?.next().is_none() {
            fs::remove_dir(path)?;
        }
        Ok(())
    }
}

impl LockedSkill {
    fn load<S: Source>(lock: &Lockfile, source: &S) -> Result<Vec<Self>, InstallError> {
        let mut skills = Vec::with_capacity(lock.entries.len());
        for entry in &lock.entries {
            let repository = source
                .repository(&entry.source, &entry.commit)
                .map_err(|err| InstallError::Fetch {
                    repo: entry.source.clone(),
                    commit: entry.commit.clone(),
                    source: Box::new(err),
                })?;
            skills.push(Self::from_entry(entry, &repository)?);
        }
        Ok(skills)
    }

    fn from_entry(entry: &LockEntry, repository: &Repository) -> Result<Self, InstallError> {
        let skill = match repository.skill(&entry.skill_path, &entry.computed_hash) {
            Ok(skill) => skill,
            Err(SkillError::MissingFolder) => {
                return Err(InstallError::missing(entry, Missing::Folder));
            }
            Err(SkillError::MissingFile) => {
                return Err(InstallError::missing(entry, Missing::File));
            }
            Err(SkillError::Symlink(path)) => {
                return Err(InstallError::Symlink {
                    skill: entry.name.clone(),
                    repo: entry.source.clone(),
                    commit: entry.commit.clone(),
                    path,
                });
            }
            Err(SkillError::HashMismatch(actual)) => {
                return Err(InstallError::HashMismatch {
                    skill: entry.name.clone(),
                    repo: entry.source.clone(),
                    commit: entry.commit.clone(),
                    expected: Box::new(entry.computed_hash.clone()),
                    actual: Box::new(actual),
                });
            }
        };
        let licenses = repository.license_files();
        if licenses.is_empty() {
            return Err(InstallError::NoLicense {
                skill: entry.name.clone(),
                repo: entry.source.clone(),
                commit: entry.commit.clone(),
            });
        }
        Ok(Self {
            folder: entry.folder_name.clone(),
            skill,
            licenses,
        })
    }
}

fn kind(path: &Path) -> io::Result<NodeKind> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(NodeKind::Symlink),
        Ok(metadata) if metadata.is_dir() => Ok(NodeKind::Directory),
        Ok(_) => Ok(NodeKind::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(NodeKind::Missing),
        Err(err) => Err(err),
    }
}

fn directory_hash(path: &Path) -> io::Result<Option<ContentHash>> {
    if !matches!(kind(path)?, NodeKind::Directory) {
        return Ok(None);
    }
    let mut files = BTreeMap::new();
    if !collect_files(path, "", &mut files)? {
        return Ok(None);
    }
    Ok(Some(ContentHash::of_files(files.iter().map(
        |(file_path, contents)| (file_path.as_str(), contents.as_slice()),
    ))))
}

/// `false` when `directory` contains anything other than regular files, such as a
/// symlink, an empty folder, or a non-UTF-8 name. Symlinks are not followed.
fn collect_files(
    directory: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> io::Result<bool> {
    let entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
    if entries.is_empty() && !prefix.is_empty() {
        return Ok(false);
    }
    for entry in entries {
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => return Ok(false),
        };
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Ok(false);
        }
        let relative = format!("{prefix}{name}");
        if file_type.is_dir() {
            if !collect_files(&entry.path(), &format!("{relative}/"), files)? {
                return Ok(false);
            }
        } else if file_type.is_file() {
            files.insert(relative, fs::read(entry.path())?);
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn mark_executable(path: &Path, executable: bool) -> io::Result<()> {
    #[cfg(unix)]
    if executable {
        let mut permissions = fs::metadata(path)?.permissions();
        let mode = permissions.mode();
        permissions.set_mode(mode | ((mode & 0o444) >> 2));
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = (path, executable);
    Ok(())
}

fn remove_leaf(path: &Path) -> io::Result<()> {
    fs::remove_file(path).or_else(|_| fs::remove_dir(path))
}

fn remove_tree(path: &Path) -> io::Result<()> {
    match kind(path)? {
        NodeKind::Symlink | NodeKind::Other => return remove_leaf(path),
        NodeKind::Missing => return Ok(()),
        NodeKind::Directory => {}
    }
    for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
        remove_tree(&entry.path())?;
    }
    fs::remove_dir(path)
}

fn is_kept(name: &OsStr, kept: &BTreeSet<FolderName>) -> bool {
    name.to_str()
        .and_then(FolderName::parse)
        .is_some_and(|folder| kept.contains(&folder))
}

/// Why an install directory was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryProblem {
    Absolute,
    Outside,
}

/// Which part of a skill path was absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    Folder,
    File,
}

/// Why an install failed. The install folder is unchanged unless a failure happens
/// while its files are being written.
#[derive(Debug)]
pub enum InstallError {
    Lock(LockError),
    Directory {
        which: &'static str,
        path: PathBuf,
        problem: DirectoryProblem,
    },
    Fetch {
        repo: GitHubRepo,
        commit: CommitSha,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    Missing {
        skill: String,
        repo: GitHubRepo,
        commit: CommitSha,
        path: Box<SkillPath>,
        missing: Missing,
    },
    Symlink {
        skill: String,
        repo: GitHubRepo,
        commit: CommitSha,
        path: String,
    },
    HashMismatch {
        skill: String,
        repo: GitHubRepo,
        commit: CommitSha,
        expected: Box<ContentHash>,
        actual: Box<ContentHash>,
    },
    NoLicense {
        skill: String,
        repo: GitHubRepo,
        commit: CommitSha,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl InstallError {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    fn missing(entry: &LockEntry, missing: Missing) -> Self {
        Self::Missing {
            skill: entry.name.clone(),
            repo: entry.source.clone(),
            commit: entry.commit.clone(),
            path: Box::new(entry.skill_path.clone()),
            missing,
        }
    }
}

impl From<LockError> for InstallError {
    fn from(err: LockError) -> Self {
        Self::Lock(err)
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(err) => err.fmt(f),
            Self::Directory {
                which,
                path,
                problem: DirectoryProblem::Absolute,
            } => write!(f, "{which} {} must be a relative path", path.display()),
            Self::Directory {
                which,
                path,
                problem: DirectoryProblem::Outside,
            } => write!(
                f,
                "{which} {} must stay inside the lock file's folder",
                path.display()
            ),
            Self::Fetch {
                repo,
                commit,
                source,
            } => write!(f, "cannot fetch {repo} at commit {commit}: {source}"),
            Self::Missing {
                skill,
                repo,
                commit,
                path,
                missing,
            } => {
                let detail = match missing {
                    Missing::Folder => "the skill folder does not exist",
                    Missing::File => "the skill folder has no SKILL.md",
                };
                write!(
                    f,
                    "skill \"{skill}\" not found at {path} in {repo} at commit {commit}: {detail}"
                )
            }
            Self::Symlink {
                skill,
                repo,
                commit,
                path,
            } if path.is_empty() => {
                write!(
                    f,
                    "skill \"{skill}\" in {repo} at commit {commit} is a symlink"
                )
            }
            Self::Symlink {
                skill,
                repo,
                commit,
                path,
            } => write!(
                f,
                "skill \"{skill}\" in {repo} at commit {commit} contains a symlink at \"{path}\""
            ),
            Self::HashMismatch {
                skill,
                repo,
                commit,
                expected,
                actual,
            } => write!(
                f,
                "skill \"{skill}\" from {repo} at commit {commit} does not match skills-lock.json: \
                 computedHash is {expected} but the fetched files hash to {actual}"
            ),
            Self::NoLicense {
                skill,
                repo,
                commit,
            } => write!(
                f,
                "skill \"{skill}\" from {repo} at commit {commit} has no license file at the repository root"
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for InstallError {}
