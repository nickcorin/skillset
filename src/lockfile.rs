use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::content_hash::ContentHash;
use crate::error::Error;
use crate::skill::Skill;

#[derive(Debug)]
pub struct Lockfile {
    pub skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Document {
    version: u64,
    skills: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
enum SourceType {
    #[serde(rename = "github")]
    Github,
}

#[derive(Deserialize)]
struct Entry {
    source: String,
    #[serde(rename = "sourceType")]
    source_type: SourceType,
    commit: String,
    #[serde(rename = "skillPath")]
    skill_path: String,
    #[serde(rename = "computedHash")]
    computed_hash: String,
    #[serde(default, rename = "skillName")]
    skill_name: Option<String>,
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Lockfile, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::Lockfile {
            path: path.to_path_buf(),
            source,
        })?;
        let document: Document =
            serde_json::from_str(&text).map_err(|err| Error::UnsupportedLock(err.to_string()))?;
        let reject = |message: String| Error::UnsupportedLock(message);
        if document.version != 2 {
            return Err(reject(format!("version {}", document.version)));
        }
        let mut skills = Vec::with_capacity(document.skills.len());
        for (name, entry) in document.skills {
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
            {
                return Err(reject(format!("invalid skill name {name:?}")));
            }
            if entry.skill_name.is_some() {
                return Err(reject(format!("skill {name} is renamed")));
            }
            match entry.source_type {
                SourceType::Github => {}
            }
            if entry.commit.is_empty() {
                return Err(reject(format!("skill {name} is missing a commit")));
            }
            if entry.skill_path.is_empty() {
                return Err(reject(format!("skill {name} is missing a skill path")));
            }
            if entry.computed_hash.is_empty() {
                return Err(reject(format!("skill {name} is missing a computed hash")));
            }
            skills.push(Skill {
                name,
                source: entry.source,
                commit: entry.commit,
                skill_path: entry.skill_path,
                hash: ContentHash::from_lock(entry.computed_hash),
            });
        }
        Ok(Lockfile { skills })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Lockfile;
    use crate::error::Error;

    fn temp_path() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("skillset-lock-{}-{nanos}-{n}", std::process::id()))
    }

    fn read_json(json: &str) -> Result<Lockfile, Error> {
        let path = temp_path();
        fs::write(&path, json).expect("write lock");
        let result = Lockfile::read(&path);
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn reads_version_two_github_lock_with_extra_fields() {
        let lock = read_json(
            r#"{
                "version": 2,
                "unknown": true,
                "skills": {
                    "find-skills": {
                        "source": "vercel-labs/skills",
                        "commit": "e7fd0e9e52f029fe87729d8185cb8caf7049f678",
                        "sourceType": "github",
                        "skillPath": "skills/find-skills/SKILL.md",
                        "computedHash": "7bc223e2aafbdbf6e50ea243ba6ef67fe432bb069ab439fea0390795dac0bf7f",
                        "sourceUrl": "https://github.com/vercel-labs/skills.git",
                        "ref": "main",
                        "installMode": "symlink"
                    }
                }
            }"#,
        )
        .expect("lock");
        assert_eq!(lock.skills.len(), 1);
        let skill = &lock.skills[0];
        assert_eq!(skill.name, "find-skills");
        assert_eq!(skill.source, "vercel-labs/skills");
        assert_eq!(skill.commit, "e7fd0e9e52f029fe87729d8185cb8caf7049f678");
        assert_eq!(skill.skill_path, "skills/find-skills/SKILL.md");
        assert_eq!(
            skill.hash.to_string(),
            "7bc223e2aafbdbf6e50ea243ba6ef67fe432bb069ab439fea0390795dac0bf7f"
        );
    }

    #[test]
    fn empty_skills_object_has_no_skills() {
        let lock = read_json(r#"{"version":2,"skills":{}}"#).expect("lock");
        assert!(lock.skills.is_empty());
    }

    #[test]
    fn missing_lock_file_names_the_path() {
        let path = temp_path();
        let err = Lockfile::read(&path).expect_err("missing");
        assert!(matches!(err, Error::Lockfile { .. }));
        assert!(err.to_string().contains("lock file not found"));
        assert!(err.to_string().contains(path.to_str().expect("utf-8")));
    }

    #[test]
    fn rejects_unsupported_locks() {
        let cases = [
            ("not json", "unsupported lock file"),
            (r#"{"version":1,"skills":{}}"#, "version 1"),
            (r#"{"version":"2","skills":{}}"#, "unsupported lock file"),
            (r#"{"version":2,"skills":[]}"#, "unsupported lock file"),
            (
                r#"{"version":2,"skills":{"demo":{"source":"owner/repo","sourceType":"gitlab","commit":"abc","skillPath":"SKILL.md","computedHash":"abc"}}}"#,
                "gitlab",
            ),
            (
                r#"{"version":2,"skills":{"demo":{"source":"owner/repo","sourceType":"github","commit":"","skillPath":"SKILL.md","computedHash":"abc"}}}"#,
                "missing a commit",
            ),
            (
                r#"{"version":2,"skills":{"demo":{"source":"owner/repo","sourceType":"github","commit":"abc","skillPath":"","computedHash":"abc"}}}"#,
                "missing a skill path",
            ),
            (
                r#"{"version":2,"skills":{"demo":{"source":"owner/repo","sourceType":"github","commit":"abc","skillPath":"SKILL.md","computedHash":""}}}"#,
                "missing a computed hash",
            ),
            (
                r#"{"version":2,"skills":{"demo":{"source":"owner/repo","sourceType":"github","commit":"abc","skillPath":"SKILL.md","computedHash":"abc","skillName":"other"}}}"#,
                "renamed",
            ),
        ];
        for (json, needle) in cases {
            let err = read_json(json).expect_err(json);
            assert!(matches!(err, Error::UnsupportedLock(_)), "{json} -> {err}");
            assert!(err.to_string().contains(needle), "{json} -> {err}");
        }
    }

    #[test]
    fn bad_skill_key_errors_and_deletes_nothing() {
        let root = temp_path();
        fs::create_dir_all(&root).expect("root");
        let install = root.join("skills");
        fs::create_dir_all(&install).expect("install");
        fs::write(install.join("keep.txt"), b"keep").expect("keep");
        fs::write(root.join("x"), b"victim").expect("victim");
        fs::write(root.join("abs-victim"), b"abs").expect("abs");
        let absolute = root.join("abs-victim");
        let absolute = absolute.to_str().expect("utf-8");
        for key in ["../x", absolute, "", ".", "..", "a/b", "a\\b"] {
            let json = serde_json::json!({
                "version": 2,
                "skills": {
                    key: {
                        "source": "owner/repo",
                        "sourceType": "github",
                        "commit": "abc",
                        "skillPath": "SKILL.md",
                        "computedHash": "abc"
                    }
                }
            });
            let path = root.join("skills-lock.json");
            fs::write(&path, json.to_string()).expect("lock");
            let parsed = Lockfile::read(&path);
            if let Ok(lock) = &parsed {
                for skill in &lock.skills {
                    let _ = skill.write(&install, &[]);
                }
            }
            let err = parsed.expect_err(key);
            assert!(matches!(err, Error::UnsupportedLock(_)), "{key} -> {err}");
            assert!(
                err.to_string().contains(&format!("{key:?}")),
                "{key} -> {err}"
            );
            assert_eq!(fs::read(install.join("keep.txt")).expect("keep"), b"keep");
            assert_eq!(fs::read(root.join("x")).expect("victim"), b"victim");
            assert_eq!(fs::read(root.join("abs-victim")).expect("abs"), b"abs");
        }
        let _ = fs::remove_dir_all(&root);
    }
}
