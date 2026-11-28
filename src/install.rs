//! The folder a lock is restored into.
//!
//! `<skill>/` is that skill's files. Everything else in the folder is left alone.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Error;
use crate::lock::Lock;
use crate::skill::Skill;

const DEFAULT_DIR: &str = ".agents/skills";

/// Where the lock's skills are written.
pub struct InstallFolder {
    path: PathBuf,
}

impl InstallFolder {
    /// Reads the lock, picks the folder, fetches each skill, checks its hash, then writes.
    pub fn install(lock_path: &Path, dir: Option<&Path>) -> Result<(), Error> {
        fn tarball(source: &str, commit: &str) -> Result<Vec<u8>, Error> {
            if let Ok(path) = env::var("SKILLSET_TARBALL") {
                return fs::read(&path).map_err(|err| Error::io("read", Path::new(&path), err));
            }
            let url = format!("https://codeload.github.com/{source}/tar.gz/{commit}");
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(300)))
                .user_agent(concat!("skillset/", env!("CARGO_PKG_VERSION")))
                .build()
                .into();
            let response = agent.get(&url).call().map_err(|err| {
                let detail = match err {
                    ureq::Error::StatusCode(404) => format!(
                        "{url} returned HTTP 404 (the repository or commit does not exist, or the repository is private)"
                    ),
                    ureq::Error::StatusCode(status) => format!("{url} returned HTTP {status}"),
                    other => format!("request to {url} failed: {other}"),
                };
                Error::new(format!(
                    "cannot fetch {source} at commit {commit}: {detail}"
                ))
            })?;
            let mut bytes = Vec::new();
            response
                .into_body()
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|err| {
                    Error::new(format!(
                        "cannot fetch {source} at commit {commit}: cannot read {url}: {err}"
                    ))
                })?;
            Ok(bytes)
        }

        let lock = Lock::read(lock_path)?;
        if lock.entries().is_empty() {
            return Ok(());
        }
        let folder = Self::choose(lock_path, dir, lock.install_dir())?;
        let mut skills = Vec::with_capacity(lock.entries().len());
        for entry in lock.entries() {
            let bytes = tarball(&entry.source, &entry.commit)?;
            let skill =
                Skill::from_tarball(&bytes, &entry.skill_path, &entry.hash).map_err(|err| {
                    Error::new(format!(
                        "skill \"{}\" from {} at commit {}: {err}",
                        entry.name, entry.source, entry.commit
                    ))
                })?;
            skills.push((entry.name.clone(), skill));
        }
        folder.write(&skills)
    }

    fn choose(
        lock_path: &Path,
        flag: Option<&Path>,
        from_lock: Option<&Path>,
    ) -> Result<Self, Error> {
        let path = if let Some(dir) = flag {
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                env::current_dir()
                    .map_err(|source| Error::io("resolve", dir, source))?
                    .join(dir)
            }
        } else {
            let base = lock_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            base.join(from_lock.unwrap_or(Path::new(DEFAULT_DIR)))
        };
        Ok(Self { path })
    }

    fn write(&self, skills: &[(String, Skill)]) -> Result<(), Error> {
        fs::create_dir_all(&self.path).map_err(|source| Error::io("create", &self.path, source))?;
        for (name, skill) in skills {
            let dest = self.path.join(name);
            if skill.matches(&dest)? {
                continue;
            }
            match fs::symlink_metadata(&dest) {
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::io("remove", &dest, source)),
                Ok(meta) if meta.is_dir() => {
                    fs::remove_dir_all(&dest)
                        .map_err(|source| Error::io("remove", &dest, source))?;
                }
                Ok(_) => {
                    fs::remove_file(&dest).map_err(|source| Error::io("remove", &dest, source))?;
                }
            }
            skill.place(&dest)?;
        }
        Ok(())
    }
}
