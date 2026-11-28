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
    _source_type: SourceType,
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
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|err| Error::UnsupportedLock(err.to_string()))?;
        if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64)
            && version != 2
        {
            return Err(Error::UnsupportedLock(format!("version {version}")));
        }
        let document: Document =
            serde_json::from_value(value).map_err(|err| Error::UnsupportedLock(err.to_string()))?;
        let reject = |message: String| Error::UnsupportedLock(message);
        if document.version != 2 {
            return Err(reject(format!("version {}", document.version)));
        }
        let mut skills = Vec::with_capacity(document.skills.len());
        for (name, entry) in document.skills {
            if entry.skill_name.is_some() {
                return Err(reject(format!("skill {name} is renamed")));
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
    use crate::skill::SkillFile;

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
    fn skill_keys_install_under_sanitized_names_and_leave_the_outside_alone() {
        let root = temp_path();
        fs::create_dir_all(&root).expect("root");
        let install = root.join("skills");
        fs::create_dir_all(&install).expect("install");
        fs::write(install.join("keep.txt"), b"keep").expect("keep");
        fs::write(root.join("victim"), b"outside").expect("victim");
        fs::write(root.join("x"), b"x").expect("x");
        let absolute = root.join("abs-victim");
        fs::write(&absolute, b"abs").expect("abs");
        let absolute = absolute.to_str().expect("utf-8").to_string();
        let cases = [
            ("My Skill", "my-skill"),
            ("../victim", "victim"),
            ("../x", "x"),
            ("C:", "c"),
            ("ev/il", "ev-il"),
            ("", "unnamed-skill"),
            (".", "unnamed-skill"),
            ("..", "unnamed-skill"),
            ("a/b", "a-b"),
            ("a\\b", "a-b"),
        ];
        for (key, folder) in cases {
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
            let lock = Lockfile::read(&path).expect(key);
            assert_eq!(lock.skills[0].name, key);
            lock.skills[0]
                .write(
                    &install,
                    &[SkillFile {
                        path: "SKILL.md".to_string(),
                        content: b"hello\n".to_vec(),
                        executable: false,
                    }],
                )
                .expect(key);
            let installed = install.join(folder).join("SKILL.md");
            assert_eq!(
                installed.parent().and_then(|path| path.parent()),
                Some(install.as_path()),
                "{key}"
            );
            assert_eq!(fs::read(&installed).expect("skill"), b"hello\n");
            assert_eq!(fs::read(install.join("keep.txt")).expect("keep"), b"keep");
            assert_eq!(fs::read(root.join("victim")).expect("victim"), b"outside");
            assert_eq!(fs::read(root.join("x")).expect("x"), b"x");
            assert_eq!(fs::read(root.join("abs-victim")).expect("abs"), b"abs");
        }
        let json = serde_json::json!({
            "version": 2,
            "skills": {
                absolute.clone(): {
                    "source": "owner/repo",
                    "sourceType": "github",
                    "commit": "abc",
                    "skillPath": "SKILL.md",
                    "computedHash": "abc"
                }
            }
        });
        fs::write(root.join("skills-lock.json"), json.to_string()).expect("lock");
        let lock = Lockfile::read(&root.join("skills-lock.json")).expect("absolute");
        assert_eq!(lock.skills[0].name, absolute);
        lock.skills[0]
            .write(
                &install,
                &[SkillFile {
                    path: "SKILL.md".to_string(),
                    content: b"absolute\n".to_vec(),
                    executable: false,
                }],
            )
            .expect("absolute write");
        let landed = fs::read_dir(&install)
            .expect("dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .any(|path| {
                fs::read(path.join("SKILL.md")).ok().as_deref() == Some(b"absolute\n".as_slice())
            });
        assert!(
            landed,
            "absolute key was not installed inside the install dir"
        );
        assert!(root.join("abs-victim").is_file());
        assert_eq!(fs::read(root.join("abs-victim")).expect("abs"), b"abs");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn version_one_lock_fails_before_entry_fields_are_parsed() {
        let err = read_json(
            r#"{"version":1,"skills":{"My Skill":{"source":"owner/repo","sourceType":"github"}}}"#,
        )
        .expect_err("version 1");
        let message = err.to_string();
        assert!(matches!(err, Error::UnsupportedLock(_)));
        assert!(message.contains("unsupported lock file"), "{message}");
        assert!(message.contains("version 1"), "{message}");
        assert!(!message.contains("commit"), "{message}");
        assert!(!message.contains("missing field"), "{message}");
    }
}
