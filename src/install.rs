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

const DEFAULT_DIR: &str = ".agents/skills";

/// Where the lock's skills are written.
pub struct InstallFolder {
    path: PathBuf,
}

impl InstallFolder {
    /// Reads the lock, picks the folder, fetches each skill, checks its hash, then writes.
    pub fn install(lock_path: &Path, dir: Option<&Path>) -> Result<(), Error> {
        Self::restore(lock_path, dir, |source, commit| {
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
        })
    }

    fn restore(
        lock_path: &Path,
        dir: Option<&Path>,
        fetch: impl Fn(&str, &str) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        let mut lock = Lock::read(lock_path)?;
        if lock.skills.is_empty() {
            return Ok(());
        }
        let path = if let Some(dir) = dir {
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
            base.join(
                lock.install_dir
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR)),
            )
        };
        let folder = Self { path };
        for skill in &mut lock.skills {
            let bytes = fetch(&skill.source, &skill.commit)?;
            skill.take(&bytes).map_err(|err| {
                Error::new(format!(
                    "skill \"{}\" from {} at commit {}: {err}",
                    skill.name, skill.source, skill.commit
                ))
            })?;
        }
        fs::create_dir_all(&folder.path)
            .map_err(|source| Error::io("create", &folder.path, source))?;
        for skill in &lock.skills {
            let dest = folder.path.join(&skill.name);
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::{Map, Value};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    use super::InstallFolder;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills")
    }

    fn tarball(_source: &str, _commit: &str) -> Result<Vec<u8>, crate::Error> {
        let path = fixtures().join("demo-skills.tar.gz");
        fs::read(&path).map_err(|err| crate::Error::io("read", &path, err))
    }

    fn install(dir: &Path, flag: Option<&Path>) {
        if let Err(err) = InstallFolder::restore(&dir.join("skills-lock.json"), flag, tarball) {
            panic!("{err}");
        }
    }

    fn copy_lock(dir: &Path) {
        fs::copy(
            fixtures().join("skills-lock.json"),
            dir.join("skills-lock.json"),
        )
        .unwrap();
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Node {
        bytes: Vec<u8>,
        executable: bool,
        modified: SystemTime,
    }

    fn tree(root: &Path) -> BTreeMap<PathBuf, Node> {
        let mut out = BTreeMap::new();
        if !root.exists() {
            return out;
        }
        let mut pending = vec![root.to_path_buf()];
        while let Some(folder) = pending.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink(), "{} is a symlink", path.display());
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                #[cfg(unix)]
                let executable = entry.metadata().unwrap().permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let executable = false;
                out.insert(
                    rel,
                    Node {
                        bytes: fs::read(&path).unwrap(),
                        executable,
                        modified: entry.metadata().unwrap().modified().unwrap(),
                    },
                );
            }
        }
        out
    }

    fn files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, bool)> {
        tree(root)
            .into_iter()
            .map(|(path, node)| (path, (node.bytes, node.executable)))
            .collect()
    }

    fn age(root: &Path) {
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for path in tree(root).keys() {
            File::options()
                .write(true)
                .open(root.join(path))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
    }

    fn in_dir(dir: &Path, body: impl FnOnce()) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _restore = Restore(previous);
        body();
    }

    #[test]
    fn an_npx_lock_installs_the_files_npx_wrote() {
        let project = TempDir::new().unwrap();
        copy_lock(project.path());

        install(project.path(), None);

        assert_eq!(
            files(&project.path().join(".agents/skills")),
            files(&fixtures().join("installed-by-npx"))
        );
    }

    #[test]
    fn the_flag_overrides_the_lock_which_overrides_the_default() {
        let default_dir = TempDir::new().unwrap();
        copy_lock(default_dir.path());
        install(default_dir.path(), None);
        assert!(
            default_dir
                .path()
                .join(".agents/skills/pdf-tools/SKILL.md")
                .is_file()
        );
        assert!(
            default_dir
                .path()
                .join(".agents/skills/release-notes/SKILL.md")
                .is_file()
        );

        let from_lock = TempDir::new().unwrap();
        copy_lock(from_lock.path());
        let mut value: Value = serde_json::from_str(
            &fs::read_to_string(from_lock.path().join("skills-lock.json")).unwrap(),
        )
        .unwrap();
        value["skillset"] = serde_json::json!({"installDir": "vendor/skills"});
        fs::write(
            from_lock.path().join("skills-lock.json"),
            format!("{value:#}\n"),
        )
        .unwrap();
        install(from_lock.path(), None);
        assert!(
            from_lock
                .path()
                .join("vendor/skills/pdf-tools/SKILL.md")
                .is_file()
        );
        assert!(!from_lock.path().join(".agents").exists());

        let from_flag = TempDir::new().unwrap();
        fs::copy(
            from_lock.path().join("skills-lock.json"),
            from_flag.path().join("skills-lock.json"),
        )
        .unwrap();
        in_dir(from_flag.path(), || {
            install(from_flag.path(), Some(Path::new("chosen")))
        });
        assert!(from_flag.path().join("chosen/pdf-tools/SKILL.md").is_file());
        assert!(
            from_flag
                .path()
                .join("chosen/release-notes/SKILL.md")
                .is_file()
        );
        assert!(!from_flag.path().join("vendor").exists());
        assert!(!from_flag.path().join(".agents").exists());
    }

    #[test]
    fn a_hash_mismatch_leaves_the_folder_unchanged() {
        let project = TempDir::new().unwrap();
        copy_lock(project.path());
        let skills = project.path().join(".agents/skills");
        fs::create_dir_all(&skills).unwrap();
        fs::write(skills.join("keep.txt"), b"keep\n").unwrap();
        let text = fs::read_to_string(project.path().join("skills-lock.json")).unwrap();
        let text = text.replace(
            "2be34cd34b36a1831c3df36e8c60fae86b73027c30a79245408706d4fadb926f",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        fs::write(project.path().join("skills-lock.json"), text).unwrap();
        let before = tree(&skills);

        let err = InstallFolder::restore(&project.path().join("skills-lock.json"), None, tarball)
            .unwrap_err();

        assert!(err.to_string().contains("computedHash"), "{err}");
        assert_eq!(tree(&skills), before);
        assert!(!skills.join("pdf-tools").exists());
    }

    #[test]
    fn a_rerun_leaves_an_unchanged_skill_and_restores_one_that_differs() {
        let project = TempDir::new().unwrap();
        copy_lock(project.path());
        install(project.path(), None);

        let skills = project.path().join(".agents/skills");
        age(&skills);
        let before = tree(&skills);
        install(project.path(), None);
        assert_eq!(tree(&skills), before);

        fs::create_dir_all(skills.join("stray")).unwrap();
        fs::write(skills.join("stray/note.txt"), b"stray\n").unwrap();
        let skill_md = skills.join("pdf-tools/SKILL.md");
        let original = fs::read(&skill_md).unwrap();
        fs::write(&skill_md, b"changed\n").unwrap();
        age(&skills);
        let notes = tree(&skills.join("release-notes"));
        let stray = tree(&skills.join("stray"));

        install(project.path(), None);

        assert_eq!(fs::read(skill_md).unwrap(), original);
        assert_eq!(tree(&skills.join("release-notes")), notes);
        assert_eq!(tree(&skills.join("stray")), stray);
    }

    #[test]
    fn a_skill_name_that_would_escape_the_install_folder_is_rejected() {
        for name in [".", "..", "nested/name", "nested\\name"] {
            let project = TempDir::new().unwrap();
            let lock = project.path().join("skills-lock.json");
            let mut skills = Map::new();
            skills.insert(
                name.to_owned(),
                serde_json::json!({
                    "source": "a/b",
                    "sourceType": "github",
                    "commit": "abc",
                    "skillPath": "SKILL.md",
                    "computedHash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                }),
            );
            fs::write(
                &lock,
                serde_json::to_string(&serde_json::json!({"version": 2, "skills": skills}))
                    .unwrap(),
            )
            .unwrap();

            let err = InstallFolder::restore(&lock, None, |_, _| panic!("fetched")).unwrap_err();

            assert!(
                err.to_string().contains("cannot be installed as a folder"),
                "{name}: {err}"
            );
            assert!(!project.path().join(".agents").exists(), "{name}");
        }
    }

    #[test]
    fn a_non_github_source_fails_at_parse() {
        let project = TempDir::new().unwrap();
        let lock = project.path().join("skills-lock.json");
        fs::write(
            &lock,
            r#"{"version":2,"skills":{"pdf":{"source":"a/b","sourceType":"local","commit":"abc","skillPath":"SKILL.md","computedHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}"#,
        )
        .unwrap();

        let err = InstallFolder::restore(&lock, None, |_, _| panic!("fetched")).unwrap_err();

        assert!(err.to_string().contains("github"), "{err}");
        assert!(!project.path().join(".agents").exists());
    }
}
