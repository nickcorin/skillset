//! The install folder.
//!
//! `<skill>/` holds that skill's files. `.licenses/<skill>/` holds the license
//! files from the root of its repository. Every other entry is removed. A symlink
//! is removed as itself and is never followed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::content_hash::ContentHash;
use crate::lock::{LockEntry, LockError, Lockfile};
use crate::repository::{Repository, SkillError};
use crate::skill::{Skill, SkillFile};
use crate::source::Source;

const DEFAULT_INSTALL_DIR: &str = ".agents/skills";
const LICENSES: &str = ".licenses";

/// The folder a lock is restored into. It is always strictly inside the lock file's
/// folder.
pub struct InstallFolder {
    /// The lock file's folder. Ancestors of this are left alone.
    base: PathBuf,
    /// Path of the install folder relative to `base`.
    relative: PathBuf,
}

struct LockedSkill {
    folder: String,
    skill: Skill,
    licenses: BTreeMap<String, SkillFile>,
}

enum Kind {
    Missing,
    Dir,
    Symlink,
    Other,
}

impl InstallFolder {
    /// Restores every skill in the lock at `lock_path`.
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
        let relative = normalize(raw).map_err(|absolute| InstallError::Directory {
            which,
            path: raw.to_path_buf(),
            absolute,
        })?;
        Ok(Self {
            base: lock_path.parent().unwrap_or(Path::new("")).to_path_buf(),
            relative,
        })
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
                self.place(
                    &root.join(&skill.folder),
                    skill.skill.files(),
                    skill.skill.hash(),
                )?;
                let license_hash = ContentHash::of_files(
                    skill
                        .licenses
                        .iter()
                        .map(|(path, file)| (path.as_str(), file.contents.as_slice())),
                );
                self.place(
                    &root.join(LICENSES).join(&skill.folder),
                    &skill.licenses,
                    &license_hash,
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
            make_dir(&prefix)?;
        }
        Ok(())
    }

    fn place(
        &self,
        path: &Path,
        files: &BTreeMap<String, SkillFile>,
        expected: &ContentHash,
    ) -> Result<(), InstallError> {
        if self
            .unchanged(path, expected)
            .map_err(|source| InstallError::io("read", path, source))?
        {
            return Ok(());
        }
        self.remove_at(path)
            .map_err(|source| InstallError::io("remove", path, source))?;
        self.write(path, files)
            .map_err(|source| InstallError::io("write", path, source))
    }

    fn unchanged(&self, path: &Path, expected: &ContentHash) -> io::Result<bool> {
        let root = self.full();
        let Ok(relative) = path.strip_prefix(&root) else {
            return Ok(false);
        };
        let mut cursor = root;
        for component in relative.components() {
            cursor.push(component);
            if !matches!(kind(&cursor)?, Kind::Dir) {
                return Ok(false);
            }
        }
        Ok(directory_hash(path)?.as_ref() == Some(expected))
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
            match kind(&prefix)? {
                Kind::Missing => return Ok(()),
                Kind::Dir if !last => {}
                Kind::Dir => return remove_tree(&prefix),
                Kind::Symlink | Kind::Other => return remove_leaf(&prefix),
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
            mark_executable(&target, file.executable)?;
        }
        Ok(())
    }

    fn ensure(&self, path: &Path) -> io::Result<()> {
        let root = self.full();
        let relative = path.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("{} is outside the install folder", path.display()))
        })?;
        make_dir(&root)?;
        let mut prefix = root;
        for component in relative.components() {
            prefix.push(component);
            make_dir(&prefix)?;
        }
        Ok(())
    }

    fn remove_others(&self, kept: &BTreeSet<String>) -> Result<(), InstallError> {
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
            let result = if name == LICENSES {
                self.retain_licenses(&path, kept)
            } else if name.to_str().is_some_and(|name| kept.contains(name)) {
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
            match kind(&self.base)? {
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
            match kind(&prefix)? {
                Kind::Dir => {}
                Kind::Missing => return Ok(None),
                Kind::Symlink | Kind::Other => {
                    remove_leaf(&prefix)?;
                    return Ok(None);
                }
            }
        }
        Ok(Some(prefix))
    }

    fn retain_licenses(&self, path: &Path, kept: &BTreeSet<String>) -> io::Result<()> {
        match kind(path)? {
            Kind::Dir => {}
            Kind::Missing => return Ok(()),
            Kind::Symlink | Kind::Other => return remove_leaf(path),
        }
        for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| kept.contains(name))
            {
                self.remove_at(&entry.path())?;
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
        lock.entries
            .iter()
            .map(|entry| {
                let repository = source
                    .repository(&entry.owner, &entry.repo, &entry.commit)
                    .map_err(|err| InstallError::Fetch {
                        source: entry.source(),
                        commit: entry.commit.clone(),
                        reason: err.to_string(),
                    })?;
                Self::from_entry(entry, &repository)
            })
            .collect()
    }

    fn from_entry(entry: &LockEntry, repository: &Repository) -> Result<Self, InstallError> {
        let detail = match repository.skill(&entry.skill_path, &entry.hash) {
            Ok(skill) => {
                let licenses = repository.license_files();
                if licenses.is_empty() {
                    return Err(entry.error("has no license file at the repository root"));
                }
                return Ok(Self {
                    folder: entry.folder.clone(),
                    skill,
                    licenses,
                });
            }
            Err(SkillError::Missing) => format!("not found at {}", entry.skill_path),
            Err(SkillError::Symlink(path)) if path.is_empty() => "is a symlink".to_owned(),
            Err(SkillError::Symlink(path)) => format!("contains a symlink at \"{path}\""),
            Err(SkillError::HashMismatch(actual)) => format!(
                "does not match skills-lock.json: computedHash is {} but the fetched files hash to {actual}",
                entry.hash
            ),
        };
        Err(entry.error(detail))
    }
}

impl LockEntry {
    fn error(&self, detail: impl Into<String>) -> InstallError {
        InstallError::Skill {
            name: self.name.clone(),
            source: self.source(),
            commit: self.commit.clone(),
            detail: detail.into(),
        }
    }
}

/// A relative path that still names a folder inside its base after `.` and `..`
/// are collapsed. `Err(true)` is absolute. `Err(false)` escapes the base or is the
/// base itself.
fn normalize(path: &Path) -> Result<PathBuf, bool> {
    if path.is_absolute() {
        return Err(true);
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(false);
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(true),
        }
    }
    if parts.is_empty() {
        return Err(false);
    }
    let mut relative = PathBuf::new();
    for part in parts {
        relative.push(part);
    }
    Ok(relative)
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
    match kind(path)? {
        Kind::Dir => Ok(()),
        Kind::Missing => fs::create_dir(path),
        Kind::Symlink | Kind::Other => {
            remove_leaf(path)?;
            fs::create_dir(path)
        }
    }
}

fn directory_hash(path: &Path) -> io::Result<Option<ContentHash>> {
    if !matches!(kind(path)?, Kind::Dir) {
        return Ok(None);
    }
    let mut files = BTreeMap::new();
    let mut pending = vec![(path.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
        if entries.is_empty() && !prefix.is_empty() {
            return Ok(None);
        }
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Ok(None);
            }
            let Ok(name) = entry.file_name().into_string() else {
                return Ok(None);
            };
            let relative = format!("{prefix}{name}");
            if file_type.is_dir() {
                pending.push((entry.path(), format!("{relative}/")));
            } else if file_type.is_file() {
                files.insert(relative, fs::read(entry.path())?);
            } else {
                return Ok(None);
            }
        }
    }
    Ok(Some(ContentHash::of_files(files.iter().map(
        |(path, contents)| (path.as_str(), contents.as_slice()),
    ))))
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
        Kind::Symlink | Kind::Other => return remove_leaf(path),
        Kind::Missing => return Ok(()),
        Kind::Dir => {}
    }
    for entry in fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()? {
        remove_tree(&entry.path())?;
    }
    fs::remove_dir(path)
}

/// Why an install failed. The install folder is unchanged unless a failure happens
/// while its files are being written.
#[derive(Debug)]
pub enum InstallError {
    Lock(LockError),
    Directory {
        which: &'static str,
        path: PathBuf,
        absolute: bool,
    },
    Fetch {
        source: String,
        commit: String,
        reason: String,
    },
    Skill {
        name: String,
        source: String,
        commit: String,
        detail: String,
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
                absolute: true,
            } => write!(f, "{which} {} must be a relative path", path.display()),
            Self::Directory {
                which,
                path,
                absolute: false,
            } => write!(
                f,
                "{which} {} must stay inside the lock file's folder",
                path.display()
            ),
            Self::Fetch {
                source,
                commit,
                reason,
            } => write!(f, "cannot fetch {source} at commit {commit}: {reason}"),
            Self::Skill {
                name,
                source,
                commit,
                detail,
            } => write!(
                f,
                "skill \"{name}\" from {source} at commit {commit} {detail}"
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
