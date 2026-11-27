//! `skillset install`: restore every skill in `skills-lock.json` at its locked commit.
//!
//! Layout of an install folder:
//!
//! ```text
//! <install folder>/
//!   <skill>/                  the skill folder, byte-identical to the source
//!   .licenses/<skill>/        license files from the root of the skill's source repository
//!   .skillset-manifest.json   the skill folders skillset placed here
//! ```

mod content;
mod manifest;
mod transaction;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use self::content::Content;
use self::manifest::Manifest;
pub use self::manifest::ManifestProblem;
use self::transaction::{Change, Transaction};
use crate::content_hash::ContentHash;
use crate::fetch::{FetchError, SourceFetcher};
use crate::lock::{CommitSha, FolderName, GitHubRepo, LockEntry, LockError, Lockfile};
use crate::snapshot::{FetchedSkill, MissingSkill, SkillFile};

/// Where skills install when neither `--dir` nor the lock names a folder,
/// relative to the lock file's folder.
pub const DEFAULT_INSTALL_DIR: &str = ".agents/skills";
/// The folder inside the install folder that holds each skill's license files.
pub const LICENSES_FOLDER: &str = ".licenses";

/// Restores the skills of one lock file into one install folder.
pub struct InstallCommand<F> {
    lock_path: PathBuf,
    install_dir_override: Option<PathBuf>,
    fetcher: F,
}

impl<F: SourceFetcher> InstallCommand<F> {
    pub fn new(lock_path: PathBuf, install_dir_override: Option<PathBuf>, fetcher: F) -> Self {
        Self {
            lock_path,
            install_dir_override,
            fetcher,
        }
    }

    /// Fetches every locked skill and checks it against its `computedHash` before
    /// changing anything, then brings the install folder in line with the lock:
    /// changed paths are replaced together, unchanged paths are not written, and
    /// skills this command placed earlier that the lock no longer lists are removed.
    pub fn install(&self) -> Result<InstallReport, InstallError> {
        let lock = Lockfile::read(&self.lock_path)?;
        let install_dir = self.install_dir(&lock);
        let skills = self.fetch_verified_skills(&lock)?;
        InstallPlan::new(install_dir, skills)?.apply()
    }

    /// `--dir` if given, else the lock's `skillset.installDir`, else
    /// [`DEFAULT_INSTALL_DIR`]; the latter two are relative to the lock's folder.
    pub fn install_dir(&self, lock: &Lockfile) -> PathBuf {
        let lock_folder = self.lock_path.parent().unwrap_or(Path::new(""));
        match (&self.install_dir_override, &lock.install_dir) {
            (Some(dir), _) => dir.clone(),
            (None, Some(dir)) => lock_folder.join(dir),
            (None, None) => lock_folder.join(DEFAULT_INSTALL_DIR),
        }
    }

    /// Fetches each repository and commit once and returns every locked skill whose
    /// files hash to the lock's `computedHash`; the first mismatch is an error.
    fn fetch_verified_skills(&self, lock: &Lockfile) -> Result<Vec<VerifiedSkill>, InstallError> {
        let mut entries_by_source: BTreeMap<(&GitHubRepo, &CommitSha), Vec<&LockEntry>> =
            BTreeMap::new();
        for entry in &lock.entries {
            entries_by_source
                .entry((&entry.source, &entry.commit))
                .or_default()
                .push(entry);
        }

        let mut verified = Vec::with_capacity(lock.entries.len());
        for ((repo, commit), entries) in entries_by_source {
            let snapshot =
                self.fetcher
                    .fetch(repo, commit)
                    .map_err(|source| InstallError::Fetch {
                        repo: repo.clone(),
                        commit: commit.clone(),
                        source: Box::new(source),
                    })?;
            let license_files = snapshot.license_files();
            for entry in entries {
                let skill = snapshot.skill_files(&entry.skill_path).map_err(|problem| {
                    InstallError::MissingSkill {
                        entry: Box::new(entry.clone()),
                        problem,
                    }
                })?;
                let actual = skill.content_hash();
                if actual != entry.computed_hash {
                    return Err(InstallError::HashMismatch {
                        entry: Box::new(entry.clone()),
                        actual,
                    });
                }
                verified.push(VerifiedSkill {
                    entry: entry.clone(),
                    skill,
                    license_files: license_files.clone(),
                });
            }
        }
        Ok(verified)
    }
}

/// A locked skill whose fetched files match its `computedHash`.
struct VerifiedSkill {
    entry: LockEntry,
    skill: FetchedSkill,
    license_files: BTreeMap<String, SkillFile>,
}

/// The changes that bring an install folder in line with the verified skills.
struct InstallPlan {
    install_dir: PathBuf,
    changes: Vec<Change>,
    outcomes: Vec<SkillOutcome>,
    warnings: Vec<InstallWarning>,
}

impl InstallPlan {
    fn new(install_dir: PathBuf, skills: Vec<VerifiedSkill>) -> Result<Self, InstallError> {
        let previous = Manifest::read(&install_dir)?;
        let mut plan = Self {
            install_dir,
            changes: Vec::new(),
            outcomes: Vec::new(),
            warnings: Vec::new(),
        };
        let mut manifest = Manifest::default();

        for VerifiedSkill {
            entry,
            skill,
            license_files,
        } in skills
        {
            let skill_folder = PathBuf::from(entry.folder_name.as_str());
            let license_folder = Path::new(LICENSES_FOLDER).join(entry.folder_name.as_str());
            if license_files.is_empty() {
                plan.warnings.push(InstallWarning::NoLicenseFile {
                    skill: entry.name.clone(),
                    repo: entry.source.clone(),
                    commit: entry.commit.clone(),
                });
            }
            let existed = plan.holds_anything(&skill_folder)?;
            let license_content =
                (!license_files.is_empty()).then_some(Content::Folder(license_files));
            let skill_changed = plan.require(skill_folder, Some(Content::Folder(skill.files)))?;
            let license_changed = plan.require(license_folder, license_content)?;
            let change = match (existed, skill_changed || license_changed) {
                (false, _) => SkillChange::Added,
                (true, true) => SkillChange::Updated,
                (true, false) => SkillChange::Unchanged,
            };
            plan.outcomes.push(SkillOutcome {
                folder_name: entry.folder_name.clone(),
                change,
                source: Some((entry.source, entry.commit)),
            });
            manifest.skills.insert(entry.folder_name);
        }

        let dropped: Vec<FolderName> = previous
            .iter()
            .flat_map(|previous| previous.skills.difference(&manifest.skills).cloned())
            .collect();
        for folder_name in dropped {
            plan.require(PathBuf::from(folder_name.as_str()), None)?;
            plan.require(Path::new(LICENSES_FOLDER).join(folder_name.as_str()), None)?;
            plan.outcomes.push(SkillOutcome {
                folder_name,
                change: SkillChange::Removed,
                source: None,
            });
        }

        let wanted = (!manifest.skills.is_empty()).then_some(manifest);
        if wanted != previous {
            plan.changes.push(Change {
                path: PathBuf::from(Manifest::FILE_NAME),
                content: wanted.map(|manifest| Content::File(manifest.to_json())),
            });
        }
        plan.outcomes
            .sort_by(|a, b| a.folder_name.cmp(&b.folder_name));
        Ok(plan)
    }

    /// Records a change unless `path` (relative to the install folder) already holds
    /// `content`, or is already absent when `content` is `None`. Returns whether a
    /// change was recorded.
    fn require(&mut self, path: PathBuf, content: Option<Content>) -> Result<bool, InstallError> {
        let up_to_date = match &content {
            Some(content) => {
                let target = self.install_dir.join(&path);
                content
                    .is_at(&target)
                    .map_err(InstallError::io("read", &target))?
            }
            None => !self.holds_anything(&path)?,
        };
        if !up_to_date {
            self.changes.push(Change { path, content });
        }
        Ok(!up_to_date)
    }

    fn holds_anything(&self, path: &Path) -> Result<bool, InstallError> {
        let target = self.install_dir.join(path);
        match fs::symlink_metadata(&target) {
            Ok(_) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(InstallError::io("inspect", &target)(source)),
        }
    }

    fn apply(self) -> Result<InstallReport, InstallError> {
        let Self {
            install_dir,
            changes,
            outcomes,
            mut warnings,
        } = self;
        if !changes.is_empty() {
            warnings.extend(Transaction::stage(&install_dir, changes)?.commit()?);
            let licenses = install_dir.join(LICENSES_FOLDER);
            let licenses_is_empty =
                fs::read_dir(&licenses).is_ok_and(|mut entries| entries.next().is_none());
            if licenses_is_empty && let Err(source) = fs::remove_dir(&licenses) {
                warnings.push(InstallWarning::NotRemoved {
                    path: licenses,
                    source,
                });
            }
        }
        Ok(InstallReport {
            install_dir,
            skills: outcomes,
            warnings,
        })
    }
}

/// What an install did.
#[derive(Debug)]
pub struct InstallReport {
    pub install_dir: PathBuf,
    /// One outcome per skill in the lock, plus one per removed skill, by folder name.
    pub skills: Vec<SkillOutcome>,
    pub warnings: Vec<InstallWarning>,
}

impl fmt::Display for InstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.skills.is_empty() {
            return writeln!(
                f,
                "no skills to install into {}",
                self.install_dir.display()
            );
        }
        for outcome in &self.skills {
            let path = self.install_dir.join(outcome.folder_name.as_str());
            write!(f, "{} {}", outcome.change, path.display())?;
            match (&outcome.change, &outcome.source) {
                (SkillChange::Added | SkillChange::Updated, Some((repo, commit))) => {
                    writeln!(f, " from {repo}@{commit}")?;
                }
                (
                    SkillChange::Added
                    | SkillChange::Updated
                    | SkillChange::Unchanged
                    | SkillChange::Removed,
                    _,
                ) => {
                    writeln!(f)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillOutcome {
    pub folder_name: FolderName,
    pub change: SkillChange,
    /// The repository and commit the skill was installed from; `None` when removed.
    pub source: Option<(GitHubRepo, CommitSha)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillChange {
    Added,
    Updated,
    Unchanged,
    Removed,
}

impl fmt::Display for SkillChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Added => "added",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Removed => "removed",
        })
    }
}

/// Something worth telling the user about an install that still succeeded.
#[derive(Debug)]
pub enum InstallWarning {
    NoLicenseFile {
        skill: String,
        repo: GitHubRepo,
        commit: CommitSha,
    },
    NotRemoved {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for InstallWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLicenseFile {
                skill,
                repo,
                commit,
            } => write!(
                f,
                "{repo} has no license file at its root at commit {commit}; skill \"{skill}\" was installed without one"
            ),
            Self::NotRemoved { path, source } => {
                write!(f, "could not remove {}: {source}", path.display())
            }
        }
    }
}

/// Why an install failed. Unless it is [`InstallError::RollbackFailed`], the install
/// folder is as it was before the install started.
#[derive(Debug)]
pub enum InstallError {
    Lock(LockError),
    Fetch {
        repo: GitHubRepo,
        commit: CommitSha,
        source: Box<FetchError>,
    },
    /// The entry's `skillPath` does not name a skill in its source.
    MissingSkill {
        entry: Box<LockEntry>,
        problem: MissingSkill,
    },
    /// The entry's source files do not hash to its `computedHash`.
    HashMismatch {
        entry: Box<LockEntry>,
        actual: ContentHash,
    },
    Manifest {
        path: PathBuf,
        problem: ManifestProblem,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    StagedContentMismatch {
        path: PathBuf,
    },
    /// A change failed and some earlier changes could not be reverted; their
    /// previous contents are kept in `staging_dir`.
    RollbackFailed {
        cause: Box<InstallError>,
        failures: Vec<(PathBuf, io::Error)>,
        staging_dir: PathBuf,
    },
}

impl InstallError {
    /// Builds the error for a failed file system `action` on `path`.
    fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_owned();
        move |source| Self::Io {
            action,
            path,
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
            Self::Fetch {
                repo,
                commit,
                source,
            } => write!(f, "cannot fetch {repo} at commit {commit}: {source}"),
            Self::MissingSkill { entry, problem } => write!(
                f,
                "skill \"{}\" not found at {} in {} at commit {}: {problem}",
                entry.name, entry.skill_path, entry.source, entry.commit
            ),
            Self::HashMismatch { entry, actual } => write!(
                f,
                "skill \"{}\" from {} at commit {} does not match skills-lock.json: \
                 computedHash is {} but the fetched files hash to {actual}",
                entry.name, entry.source, entry.commit, entry.computed_hash
            ),
            Self::Manifest { path, problem } => write!(f, "{}: {problem}", path.display()),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
            Self::StagedContentMismatch { path } => write!(
                f,
                "staged files at {} do not match the fetched files",
                path.display()
            ),
            Self::RollbackFailed {
                cause,
                failures,
                staging_dir,
            } => {
                write!(
                    f,
                    "{cause}; restoring the previous contents also failed for"
                )?;
                for (path, source) in failures {
                    write!(f, " {} ({source})", path.display())?;
                }
                write!(
                    f,
                    "; previous contents are kept in {}",
                    staging_dir.display()
                )
            }
        }
    }
}

impl std::error::Error for InstallError {}
