//! The install folder.
//!
//! `<skill>/` holds that skill's files. `.licenses/<skill>/` holds the license
//! files from the root of its repository. Every other entry is removed. A symlink
//! is removed as itself and is never followed.

use std::collections::{BTreeMap, BTreeSet};
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
const LICENSES: &str = ".licenses";

/// The folder a lock is restored into. It is always strictly inside the lock file's
/// folder.
pub struct InstallFolder {
    /// The lock file's folder, as given. Ancestors of this are left alone.
    base: PathBuf,
    /// Path of the install folder relative to `base`.
    relative: PathBuf,
}

struct LockedSkill {
    folder: FolderName,
    skill: Skill,
    licenses: BTreeMap<String, SkillFile>,
}

/// A path with `.` and `..` collapsed, without asking the filesystem.
struct LexicalPath(PathBuf);

enum Kind {
    Missing,
    Dir,
    Symlink,
    Other,
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

    fn joined(&self, path: &Path) -> Self {
        Self::normalize(&self.0.join(path))
    }

    fn relative_to(&self, child: &Self) -> Option<PathBuf> {
        child.0.strip_prefix(&self.0).ok().map(PathBuf::from)
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
        let base = lock_path.parent().unwrap_or(Path::new("")).to_path_buf();
        let lock_folder = Self::lock_folder(&base)?;
        let relative = if let Some(path) = flag {
            Self::flag_dir(path, &lock_folder)?
        } else if let Some(path) = from_lock {
            Self::lock_dir(path, &lock_folder)?
        } else {
            Self::lock_dir(Path::new(DEFAULT_INSTALL_DIR), &lock_folder)?
        };
        Ok(Self { base, relative })
    }

    fn lock_folder(base: &Path) -> Result<LexicalPath, InstallError> {
        if base.as_os_str().is_empty() {
            return LexicalPath::current_dir()
                .map_err(|source| InstallError::io("resolve", Path::new("."), source));
        }
        if base.is_absolute() {
            return Ok(LexicalPath::normalize(base));
        }
        Ok(LexicalPath::current_dir()
            .map_err(|source| InstallError::io("resolve", base, source))?
            .joined(base))
    }

    /// `--dir`, resolved against the current directory, then required to land inside
    /// the lock folder. Absolute paths are accepted.
    fn flag_dir(path: &Path, lock_folder: &LexicalPath) -> Result<PathBuf, InstallError> {
        let resolved = if path.is_absolute() {
            LexicalPath::normalize(path)
        } else {
            LexicalPath::current_dir()
                .map_err(|source| InstallError::io("resolve", path, source))?
                .joined(path)
        };
        Self::inside(lock_folder, &resolved, DirOrigin::Flag, path)
    }

    /// `skillset.installDir` or the default, relative to the lock folder.
    fn lock_dir(path: &Path, lock_folder: &LexicalPath) -> Result<PathBuf, InstallError> {
        if path.is_absolute() {
            return Err(InstallError::Directory(DirectoryError::Absolute {
                origin: DirOrigin::Lock,
                path: path.to_path_buf(),
            }));
        }
        Self::inside(
            lock_folder,
            &lock_folder.joined(path),
            DirOrigin::Lock,
            path,
        )
    }

    fn inside(
        lock_folder: &LexicalPath,
        resolved: &LexicalPath,
        origin: DirOrigin,
        given: &Path,
    ) -> Result<PathBuf, InstallError> {
        match lock_folder.relative_to(resolved) {
            Some(relative) if !relative.as_os_str().is_empty() => Ok(relative),
            _ => Err(InstallError::Directory(DirectoryError::Outside {
                origin,
                path: given.to_path_buf(),
            })),
        }
    }

    fn full(&self) -> PathBuf {
        self.base.join(&self.relative)
    }

    fn apply(&self, skills: &[LockedSkill]) -> Result<(), InstallError> {
        if !skills.is_empty() {
            self.create_root()
                .map_err(|source| InstallError::io("create", &self.full(), source))?;
            for skill in skills {
                let root = self.full();
                self.place(&root.join(skill.folder.as_str()), skill.skill.files())?;
                self.place(
                    &root.join(LICENSES).join(skill.folder.as_str()),
                    &skill.licenses,
                )?;
            }
        }
        self.remove_others(&skills.iter().map(|skill| skill.folder.clone()).collect())
    }

    /// Creates the install folder. A symlink in the relative path is replaced.
    /// `base` is only created, never replaced.
    fn create_root(&self) -> io::Result<()> {
        if !self.base.as_os_str().is_empty() {
            fs::create_dir_all(&self.base)?;
        }
        let mut prefix = self.base.clone();
        for component in self.relative.components() {
            prefix.push(component);
            Self::make_dir(&prefix)?;
        }
        Ok(())
    }

    fn place(&self, path: &Path, files: &BTreeMap<String, SkillFile>) -> Result<(), InstallError> {
        if self
            .matches_files(path, files)
            .map_err(|source| InstallError::io("read", path, source))?
        {
            return Ok(());
        }
        self.remove_at(path)
            .map_err(|source| InstallError::io("remove", path, source))?;
        self.write(path, files)
            .map_err(|source| InstallError::io("write", path, source))
    }

    /// True when `path` is a real directory holding exactly `files`, byte for byte,
    /// including the executable bit. A symlink on the way is not followed.
    fn matches_files(&self, path: &Path, files: &BTreeMap<String, SkillFile>) -> io::Result<bool> {
        let root = self.full();
        let Ok(relative) = path.strip_prefix(&root) else {
            return Ok(false);
        };
        let mut cursor = root;
        for component in relative.components() {
            cursor.push(component);
            if !matches!(Self::kind(&cursor)?, Kind::Dir) {
                return Ok(false);
            }
        }
        let mut found = BTreeMap::new();
        if !Self::collect_files(path, "", &mut found)? {
            return Ok(false);
        }
        if found.len() != files.len() {
            return Ok(false);
        }
        for (relative, file) in files {
            let Some((contents, executable)) = found.get(relative) else {
                return Ok(false);
            };
            if contents != &file.contents {
                return Ok(false);
            }
            #[cfg(unix)]
            if *executable != file.executable {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `Ok(false)` when the tree is not only files: a symlink, an empty directory,
    /// a non-UTF-8 name, or some other file type.
    fn collect_files(
        directory: &Path,
        prefix: &str,
        found: &mut BTreeMap<String, (Vec<u8>, bool)>,
    ) -> io::Result<bool> {
        let entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
        if entries.is_empty() && !prefix.is_empty() {
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
            let relative = format!("{prefix}{name}");
            if file_type.is_dir() {
                if !Self::collect_files(&entry.path(), &format!("{relative}/"), found)? {
                    return Ok(false);
                }
            } else if file_type.is_file() {
                found.insert(
                    relative,
                    (fs::read(entry.path())?, Self::is_executable(&entry.path())?),
                );
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn remove_at(&self, path: &Path) -> io::Result<()> {
        let root = self.full();
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("{} is outside the install folder", path.display()))
        })?;
        let mut prefix = root;
        let components: Vec<_> = relative.components().collect();
        for (index, component) in components.iter().enumerate() {
            prefix.push(component);
            let last = index + 1 == components.len();
            match Self::kind(&prefix)? {
                Kind::Missing => return Ok(()),
                Kind::Dir if !last => {}
                Kind::Dir => return Self::remove_tree(&prefix),
                Kind::Symlink | Kind::Other => return Self::remove_leaf(&prefix),
            }
        }
        Ok(())
    }

    fn write(&self, directory: &Path, files: &BTreeMap<String, SkillFile>) -> io::Result<()> {
        self.ensure(directory)?;
        for (relative, file) in files {
            let mut target = directory.to_path_buf();
            for part in relative.split('/') {
                target.push(part);
            }
            if let Some(parent) = target.parent() {
                self.ensure(parent)?;
            }
            fs::write(&target, &file.contents)?;
            Self::mark_executable(&target, file.executable)?;
        }
        Ok(())
    }

    fn ensure(&self, path: &Path) -> io::Result<()> {
        let root = self.full();
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("{} is outside the install folder", path.display()))
        })?;
        Self::make_dir(&root)?;
        let mut prefix = root;
        for component in relative.components() {
            prefix.push(component);
            Self::make_dir(&prefix)?;
        }
        Ok(())
    }

    fn remove_others(&self, kept: &BTreeSet<FolderName>) -> Result<(), InstallError> {
        let Some(root) = self
            .real_root()
            .map_err(|source| InstallError::io("inspect", &self.full(), source))?
        else {
            return Ok(());
        };
        for entry in fs::read_dir(&root)
            .and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
            .map_err(|source| InstallError::io("read", &root, source))?
        {
            let path = entry.path();
            let name = entry.file_name();
            let kept_name = name
                .to_str()
                .is_some_and(|name| kept.iter().any(|folder| folder.as_str() == name));
            let result = if name == LICENSES {
                self.retain_licenses(&path, kept)
            } else if kept_name {
                Ok(())
            } else {
                self.remove_at(&path)
            };
            result.map_err(|source| InstallError::io("remove", &path, source))?;
        }
        Ok(())
    }

    /// The install folder when every component below `base` is a real directory.
    /// A symlink in that relative path is removed and its target is not opened.
    fn real_root(&self) -> io::Result<Option<PathBuf>> {
        if !self.base.as_os_str().is_empty() {
            match Self::kind(&self.base)? {
                Kind::Dir | Kind::Symlink => {}
                Kind::Missing => return Ok(None),
                Kind::Other => {
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
            match Self::kind(&prefix)? {
                Kind::Dir => {}
                Kind::Missing => return Ok(None),
                Kind::Symlink | Kind::Other => {
                    Self::remove_leaf(&prefix)?;
                    return Ok(None);
                }
            }
        }
        Ok(Some(prefix))
    }

    fn retain_licenses(&self, path: &Path, kept: &BTreeSet<FolderName>) -> io::Result<()> {
        match Self::kind(path)? {
            Kind::Dir => {}
            Kind::Missing => return Ok(()),
            Kind::Symlink | Kind::Other => return Self::remove_leaf(path),
        }
        for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
            let kept_name = entry
                .file_name()
                .to_str()
                .is_some_and(|name| kept.iter().any(|folder| folder.as_str() == name));
            if !kept_name {
                self.remove_at(&entry.path())?;
            }
        }
        if fs::read_dir(path)?.next().is_none() {
            fs::remove_dir(path)?;
        }
        Ok(())
    }

    fn kind(path: &Path) -> io::Result<Kind> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Ok(Kind::Symlink),
            Ok(metadata) if metadata.is_dir() => Ok(Kind::Dir),
            Ok(_) => Ok(Kind::Other),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Kind::Missing),
            Err(err) => Err(err),
        }
    }

    fn make_dir(path: &Path) -> io::Result<()> {
        match Self::kind(path)? {
            Kind::Dir => Ok(()),
            Kind::Missing => fs::create_dir(path),
            Kind::Symlink | Kind::Other => {
                Self::remove_leaf(path)?;
                fs::create_dir(path)
            }
        }
    }

    fn is_executable(path: &Path) -> io::Result<bool> {
        #[cfg(unix)]
        {
            Ok(fs::metadata(path)?.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(false)
        }
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
        match Self::kind(path)? {
            Kind::Symlink | Kind::Other => return Self::remove_leaf(path),
            Kind::Missing => return Ok(()),
            Kind::Dir => {}
        }
        for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
            Self::remove_tree(&entry.path())?;
        }
        fs::remove_dir(path)
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
            Ok(skill) => {
                let licenses = repository.license_files();
                if licenses.is_empty() {
                    return Err(entry.failed(SkillFailure::NoLicense));
                }
                Ok(Self {
                    folder: entry.folder().clone(),
                    skill,
                    licenses,
                })
            }
            Err(err) => Err(entry.failed(SkillFailure::Repository(err))),
        }
    }
}

impl LockEntry {
    fn failed(&self, source: SkillFailure) -> InstallError {
        InstallError::Skill {
            name: self.name().to_owned(),
            repo: self.repo().clone(),
            commit: self.commit().clone(),
            source: Box::new(source),
        }
    }
}

#[derive(Debug)]
pub enum SkillFailure {
    /// The repository rejected the skill.
    Repository(SkillError),
    /// The repository root has no license file.
    NoLicense,
}

impl fmt::Display for SkillFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(source) => source.fmt(f),
            Self::NoLicense => f.write_str("has no license file at the repository root"),
        }
    }
}

impl Error for SkillFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(source) => Some(source),
            Self::NoLicense => None,
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
        source: Box<SkillFailure>,
    },
    Io {
        action: &'static str,
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
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
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
            } => write!(f, "cannot {action} {}: {source}", path.display()),
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
