use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use flate2::read::GzDecoder;
use tar::Archive;

use crate::content_hash::ContentHash;
use crate::error::Error;

#[derive(Debug)]
pub struct Skill {
    pub name: String,
    pub source: String,
    pub commit: String,
    pub skill_path: String,
    pub hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    pub path: String,
    pub content: Vec<u8>,
    pub executable: bool,
}

impl Skill {
    pub fn fetch(&self) -> Result<Vec<SkillFile>, Error> {
        let archive = self.download()?;
        self.files_from_tarball(&archive)
    }

    pub fn write(&self, dir: &Path, files: &[SkillFile]) -> Result<(), Error> {
        let dest = dir.join(&self.name);
        let io = |path: &Path, source| Error::Io {
            path: path.to_path_buf(),
            source,
        };
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|source| io(&dest, source))?;
        }
        fs::create_dir_all(&dest).map_err(|source| io(&dest, source))?;
        for file in files {
            let path = dest.join(&file.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
            }
            fs::write(&path, &file.content).map_err(|source| io(&path, source))?;
            #[cfg(unix)]
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if file.executable { 0o755 } else { 0o644 }),
            )
            .map_err(|source| io(&path, source))?;
        }
        Ok(())
    }

    fn download(&self) -> Result<Vec<u8>, Error> {
        let url = format!(
            "https://codeload.github.com/{}/tar.gz/{}",
            self.source, self.commit
        );
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent("skillset")
            .build()
            .into();
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|err| self.download_error(err))?;
        let compressed = response
            .body_mut()
            .read_to_vec()
            .map_err(|err| self.download_error(err))?;
        let mut archive = Vec::new();
        GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut archive)
            .map_err(|err| self.download_error(err))?;
        Ok(archive)
    }

    fn files_from_tarball(&self, bytes: &[u8]) -> Result<Vec<SkillFile>, Error> {
        let root = match self.skill_path.rsplit_once('/') {
            Some((root, _)) => root.to_string(),
            None => String::new(),
        };
        let mut archive = Archive::new(bytes);
        let mut files = Vec::new();
        for entry in archive.entries().map_err(|err| self.download_error(err))? {
            let mut entry = entry.map_err(|err| self.download_error(err))?;
            let raw = entry.path().map_err(|err| self.download_error(err))?;
            let Some(path) = self
                .skill_file(&root, raw.as_ref())
                .map_err(|err| self.download_error(err))?
            else {
                continue;
            };
            if self.excluded(&path) {
                continue;
            }
            if entry.header().entry_type().is_symlink() {
                return Err(Error::Symlink {
                    skill: self.name.clone(),
                    path,
                });
            }
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let mode = entry
                .header()
                .mode()
                .map_err(|err| self.download_error(err))?;
            let mut content = Vec::new();
            entry
                .read_to_end(&mut content)
                .map_err(|err| self.download_error(err))?;
            files.push(SkillFile {
                path,
                content,
                executable: mode & 0o111 != 0,
            });
        }
        files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        if !files.iter().any(|file| file.path == "SKILL.md") {
            return Err(Error::SkillNotFound {
                skill: self.name.clone(),
                path: self.skill_path.clone(),
            });
        }
        let actual = ContentHash::of(&files);
        if actual != self.hash {
            return Err(Error::HashMismatch {
                skill: self.name.clone(),
                expected: self.hash.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(files)
    }

    fn download_error(&self, err: impl std::fmt::Display) -> Error {
        Error::Download {
            skill: self.name.clone(),
            message: err.to_string(),
        }
    }

    fn skill_file(&self, root: &str, path: &Path) -> Result<Option<String>, String> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => match part.to_str() {
                    Some(text) => parts.push(text),
                    None => return Err("archive path is not utf-8".to_string()),
                },
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(format!("unsafe path {}", path.display()));
                }
            }
        }
        let [_, rest @ ..] = parts.as_slice() else {
            return Ok(None);
        };
        if rest.is_empty() {
            return Ok(None);
        }
        let repo_path = rest.join("/");
        if root.is_empty() {
            Ok(Some(repo_path))
        } else {
            Ok(repo_path
                .strip_prefix(&format!("{root}/"))
                .map(str::to_string))
        }
    }

    fn excluded(&self, path: &str) -> bool {
        let mut parts = path.split('/');
        let name = parts.next_back().unwrap_or("");
        name == "metadata.json"
            || parts.any(|part| {
                matches!(
                    part,
                    ".git" | "__pycache__" | "__pypackages__" | "metadata.json"
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use tar::{Builder, EntryType, Header};

    use super::{Skill, SkillFile};
    use crate::content_hash::ContentHash;
    use crate::error::Error;

    const TOP: &str = "repo-0123456789abcdef0123456789abcdef01234567";

    enum Packed {
        File {
            path: String,
            bytes: Vec<u8>,
            mode: u32,
        },
        Link {
            path: String,
            target: String,
        },
    }

    fn file(path: &str, bytes: &[u8], mode: u32) -> Packed {
        Packed::File {
            path: format!("{TOP}/{path}"),
            bytes: bytes.to_vec(),
            mode,
        }
    }

    fn link(path: &str, target: &str) -> Packed {
        Packed::Link {
            path: format!("{TOP}/{path}"),
            target: target.to_string(),
        }
    }

    fn archive(entries: &[Packed]) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        for entry in entries {
            let mut header = Header::new_gnu();
            match entry {
                Packed::File { path, bytes, mode } => {
                    header.set_path(path).expect("path");
                    header.set_size(bytes.len() as u64);
                    header.set_mode(*mode);
                    header.set_entry_type(EntryType::Regular);
                    header.set_cksum();
                    builder.append(&header, bytes.as_slice()).expect("file");
                }
                Packed::Link { path, target } => {
                    header.set_path(path).expect("path");
                    header.set_link_name(target).expect("link");
                    header.set_entry_type(EntryType::Symlink);
                    header.set_size(0);
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).expect("link");
                }
            }
        }
        builder.into_inner().expect("tar bytes")
    }

    fn sample(skill_path: &str) -> Skill {
        Skill {
            name: "demo".to_string(),
            source: "owner/repo".to_string(),
            commit: "abc".to_string(),
            skill_path: skill_path.to_string(),
            hash: ContentHash::from_lock("unused".to_string()),
        }
    }

    fn read_with_hash(skill_path: &str, bytes: &[u8], expected: &[SkillFile]) -> Vec<SkillFile> {
        let mut skill = sample(skill_path);
        skill.hash = ContentHash::of(expected);
        let files = skill.files_from_tarball(bytes).expect("files");
        assert_eq!(files, expected);
        files
    }

    fn skill_file(path: &str, content: &[u8], executable: bool) -> SkillFile {
        SkillFile {
            path: path.to_string(),
            content: content.to_vec(),
            executable,
        }
    }

    fn temp_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("skillset-write-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("temp");
        path
    }

    fn mode_of(path: &Path) -> u32 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(path).expect("meta").permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            0
        }
    }

    #[test]
    fn nested_skill_omits_files_outside_its_root() {
        let expected = [
            skill_file("SKILL.md", b"yes\n", false),
            skill_file("docs/a.md", b"a\n", false),
        ];
        let bytes = archive(&[
            file("README.md", b"no\n", 0o644),
            file("skill/SKILL.md", b"yes\n", 0o644),
            file("skill/docs/a.md", b"a\n", 0o644),
        ]);
        read_with_hash("skill/SKILL.md", &bytes, &expected);
    }

    #[test]
    fn skill_at_archive_root_includes_repo_files() {
        let expected = [
            skill_file("SKILL.md", b"yes\n", false),
            skill_file("docs/a.md", b"a\n", false),
        ];
        let bytes = archive(&[
            file("SKILL.md", b"yes\n", 0o644),
            file("docs/a.md", b"a\n", 0o644),
        ]);
        read_with_hash("SKILL.md", &bytes, &expected);
    }

    #[test]
    fn exclusions_skip_metadata_and_selected_directories() {
        let expected = [
            skill_file("SKILL.md", b"skill\n", false),
            skill_file("docs/guide.md", b"guide\n", false),
            skill_file("node_modules/pkg/index.js", b"js\n", false),
            skill_file("pkg/.git", b"keep\n", false),
            skill_file("realdir/secret.txt", b"secret\n", false),
        ];
        let bytes = archive(&[
            file("skill/SKILL.md", b"skill\n", 0o644),
            file("skill/metadata.json", b"{}\n", 0o644),
            file("skill/sub/metadata.json", b"{}\n", 0o644),
            file("skill/meta/metadata.json/hidden.txt", b"hidden\n", 0o644),
            file("skill/.git/HEAD", b"ref\n", 0o644),
            file("skill/pkg/.git", b"keep\n", 0o644),
            file("skill/__pypackages__/lib.py", b"py\n", 0o644),
            file("skill/__pycache__/mod.pyc", b"pyc\n", 0o644),
            file("skill/node_modules/pkg/index.js", b"js\n", 0o644),
            file("skill/realdir/secret.txt", b"secret\n", 0o644),
            file("skill/docs/guide.md", b"guide\n", 0o644),
        ]);
        read_with_hash("skill/SKILL.md", &bytes, &expected);
    }

    #[test]
    fn hash_mismatch_leaves_the_skill_folder_untouched() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("demo")).expect("demo");
        fs::write(dir.join("demo/keep.txt"), b"keep").expect("keep");
        fs::write(dir.join("sibling.txt"), b"side").expect("side");
        let bytes = archive(&[file("skill/SKILL.md", b"# hi\n", 0o644)]);
        let mut skill = sample("skill/SKILL.md");
        skill.hash = ContentHash::from_lock(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        );
        let err = skill.files_from_tarball(&bytes).expect_err("mismatch");
        match err {
            Error::HashMismatch {
                skill,
                expected,
                actual,
            } => {
                assert_eq!(skill, "demo");
                assert_eq!(
                    expected,
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                );
                assert_eq!(
                    actual,
                    "93c36952d8843b33e68ae970ebabcc03e1687c9bb0dd1ecaf357060318952f55"
                );
            }
            other => panic!("{other}"),
        }
        assert_eq!(fs::read(dir.join("demo/keep.txt")).expect("keep"), b"keep");
        assert_eq!(fs::read(dir.join("sibling.txt")).expect("side"), b"side");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlink_in_the_skill_is_an_error() {
        let bytes = archive(&[
            file("skill/SKILL.md", b"# hi\n", 0o644),
            link("skill/docs/b.md", "../SKILL.md"),
        ]);
        let err = sample("skill/SKILL.md")
            .files_from_tarball(&bytes)
            .expect_err("symlink");
        match err {
            Error::Symlink { skill, path } => {
                assert_eq!(skill, "demo");
                assert_eq!(path, "docs/b.md");
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn missing_skill_file_names_the_skill_and_skill_path() {
        let bytes = archive(&[file("skill/docs/a.md", b"a\n", 0o644)]);
        let err = sample("skills/find-skills/SKILL.md")
            .files_from_tarball(&bytes)
            .expect_err("missing");
        match err {
            Error::SkillNotFound { skill, path } => {
                assert_eq!(skill, "demo");
                assert_eq!(path, "skills/find-skills/SKILL.md");
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn write_replaces_only_that_skill_folder() {
        let dir = temp_dir();
        let sibling = dir.join("sibling.txt");
        fs::write(&sibling, b"sibling").expect("sibling");
        let other = dir.join("other");
        fs::create_dir_all(&other).expect("other");
        let other_file = other.join("keep.txt");
        fs::write(&other_file, b"keep").expect("keep");
        let demo = dir.join("demo");
        fs::create_dir_all(&demo).expect("demo");
        fs::write(demo.join("old.txt"), b"old").expect("old");

        sample("skill/SKILL.md")
            .write(
                &dir,
                &[
                    skill_file("SKILL.md", b"new\n", false),
                    skill_file("bin/run", b"#!/bin/sh\n", true),
                ],
            )
            .expect("write");

        assert!(!demo.join("old.txt").exists());
        assert_eq!(fs::read(&sibling).expect("read"), b"sibling");
        assert_eq!(fs::read(&other_file).expect("read"), b"keep");
        assert_eq!(fs::read(demo.join("SKILL.md")).expect("read"), b"new\n");
        assert_eq!(
            fs::read(demo.join("bin/run")).expect("read"),
            b"#!/bin/sh\n"
        );
        assert_eq!(mode_of(&demo.join("SKILL.md")), 0o644);
        assert_eq!(mode_of(&demo.join("bin/run")), 0o755);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_write_keeps_bytes_and_executable_bit() {
        let dir = temp_dir();
        let skill = sample("skill/SKILL.md");
        let files = [
            skill_file("SKILL.md", b"new\n", false),
            skill_file("bin/run", b"#!/bin/sh\n", true),
        ];
        skill.write(&dir, &files).expect("first");
        skill.write(&dir, &files).expect("second");
        assert_eq!(fs::read(dir.join("demo/SKILL.md")).expect("read"), b"new\n");
        assert_eq!(
            fs::read(dir.join("demo/bin/run")).expect("read"),
            b"#!/bin/sh\n"
        );
        assert_eq!(mode_of(&dir.join("demo/SKILL.md")), 0o644);
        assert_eq!(mode_of(&dir.join("demo/bin/run")), 0o755);
        let _ = fs::remove_dir_all(&dir);
    }
}
