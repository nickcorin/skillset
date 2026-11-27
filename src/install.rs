//! The install folder.
//!
//! `<skill>/` holds that skill's files. Everything else in the install folder is
//! left alone. A symlink on the way to that folder is followed. A file on that
//! path is an error, and the file is left in place. Replacing a skill folder
//! unlinks a symlink instead of following it.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::lock::{FolderName, LockEntry, LockError, Lockfile};
use crate::repository::{Repository, SkillError};
use crate::skill::{Skill, SkillFile};
use crate::source::{CommitSha, GitHubRepo, Source};

const DEFAULT_INSTALL_DIR: &str = ".agents/skills";

/// The folder a lock is restored into. It is always strictly inside the lock file's
/// folder.
pub struct InstallFolder {
    /// The lock file's folder, as given. `None` when the lock file is in the
    /// current directory. Ancestors of this are left alone.
    base: Option<PathBuf>,
    /// Path of the install folder relative to `base`.
    relative: PathBuf,
}

struct LockedSkill {
    folder: FolderName,
    skill: Skill,
}

/// A path with `.` and `..` collapsed, without asking the filesystem.
struct LexicalPath(PathBuf);

enum Kind {
    Missing,
    Dir,
    Symlink,
    Other,
}

/// A filesystem path the install reads or writes.
struct EntryPath<'a>(&'a Path);

#[derive(Clone, Copy, Debug)]
pub enum IoAction {
    Resolve,
    Create,
    Read,
    Remove,
    Write,
}

impl IoAction {
    fn verb(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Create => "create",
            Self::Read => "read",
            Self::Remove => "remove",
            Self::Write => "write",
        }
    }
}

impl EntryPath<'_> {
    fn kind(&self) -> io::Result<Kind> {
        match fs::symlink_metadata(self.0) {
            Ok(metadata) if metadata.file_type().is_symlink() => Ok(Kind::Symlink),
            Ok(metadata) if metadata.is_dir() => Ok(Kind::Dir),
            Ok(_) => Ok(Kind::Other),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Kind::Missing),
            Err(err) => Err(err),
        }
    }

    /// A symlink is left in place so later writes follow it. A file makes
    /// `create_dir` fail, which leaves the file in place.
    fn make_dir(&self) -> io::Result<()> {
        match self.kind()? {
            Kind::Dir | Kind::Symlink => Ok(()),
            Kind::Missing | Kind::Other => fs::create_dir(self.0),
        }
    }

    fn is_executable(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            Ok(fs::metadata(self.0)?.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            Ok(false)
        }
    }

    fn mark_executable(&self, executable: bool) -> io::Result<()> {
        #[cfg(unix)]
        if executable {
            let mut permissions = fs::metadata(self.0)?.permissions();
            let mode = permissions.mode();
            permissions.set_mode(mode | ((mode & 0o444) >> 2));
            fs::set_permissions(self.0, permissions)?;
        }
        #[cfg(not(unix))]
        let _ = executable;
        Ok(())
    }

    fn remove_leaf(&self) -> io::Result<()> {
        fs::remove_file(self.0).or_else(|_| fs::remove_dir(self.0))
    }

    fn remove_tree(&self) -> io::Result<()> {
        match self.kind()? {
            Kind::Symlink | Kind::Other => return self.remove_leaf(),
            Kind::Missing => return Ok(()),
            Kind::Dir => {}
        }
        for entry in fs::read_dir(self.0)?.collect::<io::Result<Vec<_>>>()? {
            EntryPath(&entry.path()).remove_tree()?;
        }
        fs::remove_dir(self.0)
    }

    /// `Ok(false)` when the tree is not only files: a symlink, an empty directory,
    /// a non-UTF-8 name, or some other file type. `prefix` is `None` at the skill root.
    fn collect_files(
        &self,
        prefix: Option<&str>,
        found: &mut BTreeMap<String, SkillFile>,
    ) -> io::Result<bool> {
        let entries = fs::read_dir(self.0)?.collect::<io::Result<Vec<_>>>()?;
        if entries.is_empty() && prefix.is_some() {
            return Ok(false);
        }
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Ok(false);
            }
            let Ok(name) = entry.file_name().into_string() else {
                return Ok(false);
            };
            let relative = match prefix {
                Some(prefix) => format!("{prefix}/{name}"),
                None => name,
            };
            if file_type.is_dir() {
                if !EntryPath(&entry.path()).collect_files(Some(&relative), found)? {
                    return Ok(false);
                }
            } else if file_type.is_file() {
                found.insert(
                    relative,
                    SkillFile {
                        contents: fs::read(entry.path())?,
                        executable: EntryPath(&entry.path()).is_executable()?,
                    },
                );
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// True when this path is a real directory holding exactly `files`, byte for
    /// byte, including the executable bit. A symlink is not a directory.
    fn holds(&self, files: &BTreeMap<String, SkillFile>) -> io::Result<bool> {
        if !matches!(self.kind()?, Kind::Dir) {
            return Ok(false);
        }
        let mut found = BTreeMap::new();
        if !self.collect_files(None, &mut found)? {
            return Ok(false);
        }
        if found.len() != files.len() {
            return Ok(false);
        }
        for (relative, file) in files {
            let Some(found_file) = found.get(relative) else {
                return Ok(false);
            };
            if found_file.contents != file.contents {
                return Ok(false);
            }
            #[cfg(unix)]
            if found_file.executable != file.executable {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// This directory was just removed, so it is created empty and each file's
    /// parent is created under it.
    fn write(&self, files: &BTreeMap<String, SkillFile>) -> io::Result<()> {
        fs::create_dir(self.0)?;
        for (relative, file) in files {
            let mut target = self.0.to_path_buf();
            for part in relative.split('/') {
                target.push(part);
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, &file.contents)?;
            EntryPath(&target).mark_executable(file.executable)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DirOrigin {
    Flag,
    Lock,
}

impl DirOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Flag => "--dir",
            Self::Lock => "skillset.installDir",
        }
    }
}

impl LexicalPath {
    fn normalize(path: &Path) -> Self {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        Self(out)
    }

    fn current_dir() -> io::Result<Self> {
        Ok(Self::normalize(&env::current_dir()?))
    }

    fn from_lock_base(base: Option<&Path>) -> Result<Self, InstallError> {
        match base {
            None => Self::current_dir()
                .map_err(|source| InstallError::io(IoAction::Resolve, Path::new("."), source)),
            Some(base) if base.is_absolute() => Ok(Self::normalize(base)),
            Some(base) => Ok(Self::current_dir()
                .map_err(|source| InstallError::io(IoAction::Resolve, base, source))?
                .joined(base)),
        }
    }

    fn joined(&self, path: &Path) -> Self {
        Self::normalize(&self.0.join(path))
    }

    fn relative_to(&self, child: &Self) -> Option<PathBuf> {
        child.0.strip_prefix(&self.0).ok().map(PathBuf::from)
    }

    /// `--dir`, resolved against the current directory, then required to land inside
    /// this lock folder. Absolute paths are accepted.
    fn flag_dir(&self, path: &Path) -> Result<PathBuf, InstallError> {
        let resolved = if path.is_absolute() {
            Self::normalize(path)
        } else {
            Self::current_dir()
                .map_err(|source| InstallError::io(IoAction::Resolve, path, source))?
                .joined(path)
        };
        self.inside(&resolved, DirOrigin::Flag, path)
    }

    /// `skillset.installDir` or the default, relative to this lock folder.
    fn lock_dir(&self, path: &Path) -> Result<PathBuf, InstallError> {
        if path.is_absolute() {
            return Err(InstallError::Directory(DirectoryError::Absolute {
                origin: DirOrigin::Lock,
                path: path.to_path_buf(),
            }));
        }
        self.inside(&self.joined(path), DirOrigin::Lock, path)
    }

    fn inside(
        &self,
        resolved: &Self,
        origin: DirOrigin,
        given: &Path,
    ) -> Result<PathBuf, InstallError> {
        match self.relative_to(resolved) {
            Some(relative) if !relative.as_os_str().is_empty() => Ok(relative),
            _ => Err(InstallError::Directory(DirectoryError::Outside {
                origin,
                path: given.to_path_buf(),
            })),
        }
    }
}

impl InstallFolder {
    /// Restores every skill in the lock at `lock_path`.
    ///
    /// `dir` is resolved against the current directory. `skillset.installDir` is
    /// relative to the lock file's folder. When neither is set, skills install into
    /// `.agents/skills` there. The folder must be strictly inside the lock file's
    /// folder. Every skill is read and checked before that folder changes.
    pub fn install<S: Source>(
        lock_path: &Path,
        dir: Option<&Path>,
        source: &S,
    ) -> Result<(), InstallError> {
        let lock = Lockfile::read(lock_path)?;
        let folder = Self::resolve(lock_path, dir, lock.install_dir())?;
        let skills = LockedSkill::load(&lock, source)?;
        folder.apply(&skills)
    }

    fn resolve(
        lock_path: &Path,
        flag: Option<&Path>,
        from_lock: Option<&Path>,
    ) -> Result<Self, InstallError> {
        let base = lock_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::from);
        let lock_folder = LexicalPath::from_lock_base(base.as_deref())?;
        let relative = if let Some(path) = flag {
            lock_folder.flag_dir(path)?
        } else if let Some(path) = from_lock {
            lock_folder.lock_dir(path)?
        } else {
            lock_folder.lock_dir(Path::new(DEFAULT_INSTALL_DIR))?
        };
        Ok(Self { base, relative })
    }

    fn full(&self) -> PathBuf {
        match &self.base {
            Some(base) => base.join(&self.relative),
            None => self.relative.clone(),
        }
    }

    fn apply(&self, skills: &[LockedSkill]) -> Result<(), InstallError> {
        if skills.is_empty() {
            return Ok(());
        }
        self.create_root()?;
        for skill in skills {
            let path = self.full().join(skill.folder.as_str());
            let entry = EntryPath(&path);
            let files = skill.skill.files();
            if entry
                .holds(files)
                .map_err(|source| InstallError::io(IoAction::Read, &path, source))?
            {
                continue;
            }
            entry
                .remove_tree()
                .map_err(|source| InstallError::io(IoAction::Remove, &path, source))?;
            entry
                .write(files)
                .map_err(|source| InstallError::io(IoAction::Write, &path, source))?;
        }
        Ok(())
    }

    /// Creates the install folder. A symlink on the way is followed. A file on
    /// the way makes this fail, and the error names that file's resolved path.
    /// `base` is only created, never replaced.
    fn create_root(&self) -> Result<(), InstallError> {
        if let Some(base) = &self.base {
            fs::create_dir_all(base)
                .map_err(|source| InstallError::io(IoAction::Create, base, source))?;
        }
        let mut prefix = self.base.clone().unwrap_or_default();
        for component in self.relative.components() {
            prefix.push(component);
            if let Err(source) = EntryPath(&prefix).make_dir() {
                let path = match EntryPath(&prefix).kind() {
                    Ok(Kind::Other) => prefix,
                    _ => self.full(),
                };
                return Err(InstallError::io(IoAction::Create, &path, source));
            }
        }
        Ok(())
    }
}

impl LockedSkill {
    fn load<S: Source>(lock: &Lockfile, source: &S) -> Result<Vec<Self>, InstallError> {
        lock.entries()
            .iter()
            .map(|entry| {
                let repository =
                    source
                        .repository(entry.repo(), entry.commit())
                        .map_err(|err| InstallError::Fetch {
                            repo: entry.repo().clone(),
                            commit: entry.commit().clone(),
                            source: Box::new(err),
                        })?;
                Self::from_entry(entry, &repository)
            })
            .collect()
    }

    fn from_entry(entry: &LockEntry, repository: &Repository) -> Result<Self, InstallError> {
        match repository.skill(entry.skill_path(), entry.hash()) {
            Ok(skill) => Ok(Self {
                folder: entry.folder().clone(),
                skill,
            }),
            Err(source) => Err(InstallError::Skill {
                name: entry.name().to_owned(),
                repo: entry.repo().clone(),
                commit: entry.commit().clone(),
                source: Box::new(source),
            }),
        }
    }
}

/// Why an install failed. The install folder is unchanged unless a failure happens
/// while its files are being written.
#[derive(Debug)]
pub enum InstallError {
    Lock(LockError),
    Directory(DirectoryError),
    Fetch {
        repo: GitHubRepo,
        commit: CommitSha,
        source: Box<dyn Error + Send + Sync>,
    },
    Skill {
        name: String,
        repo: GitHubRepo,
        commit: CommitSha,
        source: Box<SkillError>,
    },
    Io {
        action: IoAction,
        path: PathBuf,
        source: io::Error,
    },
}

#[derive(Debug)]
pub enum DirectoryError {
    /// `skillset.installDir` is absolute.
    Absolute { origin: DirOrigin, path: PathBuf },
    /// The path is the lock folder itself or leaves that folder.
    Outside { origin: DirOrigin, path: PathBuf },
}

impl InstallError {
    fn io(action: IoAction, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl From<LockError> for InstallError {
    fn from(err: LockError) -> Self {
        Self::Lock(err)
    }
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute { origin, path } => {
                write!(
                    f,
                    "{} {} must be a relative path",
                    origin.label(),
                    path.display()
                )
            }
            Self::Outside { origin, path } => write!(
                f,
                "{} {} must stay inside the lock file's folder",
                origin.label(),
                path.display()
            ),
        }
    }
}

impl Error for DirectoryError {}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(source) => source.fmt(f),
            Self::Directory(source) => source.fmt(f),
            Self::Fetch {
                repo,
                commit,
                source,
            } => write!(f, "cannot fetch {repo} at commit {commit}: {source}"),
            Self::Skill {
                name,
                repo,
                commit,
                source,
            } => write!(
                f,
                "skill \"{name}\" from {repo} at commit {commit} {source}"
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {} {}: {source}", action.verb(), path.display()),
        }
    }
}

impl Error for InstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lock(source) => Some(source),
            Self::Directory(source) => Some(source),
            Self::Fetch { source, .. } => Some(source.as_ref()),
            Self::Skill { source, .. } => Some(source.as_ref()),
            Self::Io { source, .. } => Some(source),
        }
    }
}
