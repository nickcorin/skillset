//! The files of one repository at one commit.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read};

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::lock::SkillPath;
use crate::skill::{Skill, SkillFile};

const EXCLUDED_FOLDERS: [&str; 3] = [".git", "__pycache__", "__pypackages__"];

#[derive(Clone, Debug)]
enum Node {
    File(SkillFile),
    Symlink,
}

/// Every file and symlink in a repository, keyed by `/`-separated path.
/// Folders are implied by the paths beneath them.
#[derive(Clone, Debug, Default)]
pub struct Repository {
    nodes: BTreeMap<String, Node>,
}

impl Repository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, path: &str, file: SkillFile) -> Result<(), ArchiveError> {
        self.insert(path, Node::File(file))
    }

    pub fn add_symlink(&mut self, path: &str) -> Result<(), ArchiveError> {
        self.insert(path, Node::Symlink)
    }

    /// Reads a gzipped tarball whose entries sit under one top-level folder, which
    /// is how GitHub lays out a commit. A symlink is recorded and its target is
    /// not resolved.
    pub fn from_tar_gz(reader: impl Read) -> Result<Self, ArchiveError> {
        let mut archive = tar::Archive::new(GzDecoder::new(reader));
        let mut repository = Self::new();
        let mut top_folder: Option<String> = None;
        for entry in archive.entries().map_err(ArchiveError::Read)? {
            let mut entry = entry.map_err(ArchiveError::Read)?;
            let entry_type = entry.header().entry_type();
            if entry_type.is_pax_global_extensions() {
                continue;
            }
            let path = {
                let bytes = entry.path_bytes();
                let path = std::str::from_utf8(&bytes).map_err(|_| {
                    ArchiveError::NonUtf8Path(String::from_utf8_lossy(&bytes).into_owned())
                })?;
                path.trim_end_matches('/').to_owned()
            };
            if !plain_relative(&path) {
                return Err(ArchiveError::InvalidPath(path));
            }
            let (top, inner) = path.split_once('/').unwrap_or((path.as_str(), ""));
            match &top_folder {
                None => top_folder = Some(top.to_owned()),
                Some(expected) if expected != top => {
                    return Err(ArchiveError::MultipleTopFolders {
                        first: expected.clone(),
                        second: top.to_owned(),
                    });
                }
                Some(_) => {}
            }
            if entry_type.is_dir() || inner.is_empty() {
                continue;
            }
            let inner = inner.to_owned();
            if entry_type.is_symlink() {
                let target = entry
                    .link_name_bytes()
                    .ok_or_else(|| ArchiveError::MissingLinkTarget(inner.clone()))?;
                String::from_utf8(target.into_owned()).map_err(|err| {
                    ArchiveError::NonUtf8Path(String::from_utf8_lossy(err.as_bytes()).into_owned())
                })?;
                repository.nodes.insert(inner, Node::Symlink);
            } else if entry_type.is_file() || entry_type.is_contiguous() {
                let mode = entry.header().mode().map_err(ArchiveError::Read)?;
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(ArchiveError::Read)?;
                repository.nodes.insert(
                    inner,
                    Node::File(SkillFile {
                        contents,
                        executable: mode & 0o111 != 0,
                    }),
                );
            } else {
                return Err(ArchiveError::UnsupportedEntry(inner));
            }
        }
        Ok(repository)
    }

    /// The skill whose `SKILL.md` is at `skill_path`.
    ///
    /// `metadata.json`, `.git`, `__pycache__` and `__pypackages__` are left out, as
    /// they are in the lock's `computedHash`. A symlink anywhere under the skill
    /// folder is an error; it is not followed.
    pub fn skill(
        &self,
        skill_path: &SkillPath,
        expected: &ContentHash,
    ) -> Result<Skill, SkillError> {
        let folder = skill_path.folder();
        if !folder.is_empty() {
            match self.nodes.get(folder) {
                Some(Node::Symlink) => return Err(SkillError::Symlink(String::new())),
                Some(Node::File(_)) => return Err(SkillError::MissingFolder),
                None if !self.is_directory(folder) => return Err(SkillError::MissingFolder),
                None => {}
            }
        }

        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let mut files = BTreeMap::new();
        let mut symlink = None;
        for (path, node) in &self.nodes {
            let Some(relative) = path.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            match node {
                Node::Symlink => {
                    if symlink.is_none() {
                        symlink = Some(relative.to_owned());
                    }
                }
                Node::File(file) if !excluded(relative) => {
                    files.insert(relative.to_owned(), file.clone());
                }
                Node::File(_) => {}
            }
        }
        if let Some(path) = symlink {
            return Err(SkillError::Symlink(path));
        }
        if !files.contains_key(skill_path.file_name()) {
            return Err(SkillError::MissingFile);
        }
        Skill::from_files(files, expected).map_err(SkillError::HashMismatch)
    }

    /// License files at the repository root. A symlink is not a license file.
    ///
    /// Names, in any letter case: `LICENSE`, `LICENCE`, `COPYING` or `UNLICENSE`
    /// with any extension, or `LICENSE-<name>` / `LICENCE-<name>` with no extension,
    /// `.md` or `.txt`.
    pub fn license_files(&self) -> BTreeMap<String, SkillFile> {
        self.nodes
            .iter()
            .filter_map(|(path, node)| {
                if path.contains('/') || !is_license_name(path) {
                    return None;
                }
                match node {
                    Node::File(file) => Some((path.clone(), file.clone())),
                    Node::Symlink => None,
                }
            })
            .collect()
    }

    fn insert(&mut self, path: &str, node: Node) -> Result<(), ArchiveError> {
        if !plain_relative(path) {
            return Err(ArchiveError::InvalidPath(path.to_owned()));
        }
        self.nodes.insert(path.to_owned(), node);
        Ok(())
    }

    fn is_directory(&self, folder: &str) -> bool {
        let prefix = format!("{folder}/");
        self.nodes.keys().any(|path| path.starts_with(&prefix))
    }
}

fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains('\\')
                && !part.contains(':')
        })
}

fn excluded(relative: &str) -> bool {
    let mut parts = relative.split('/');
    let name = parts.next_back().unwrap_or(relative);
    parts.any(|part| part == "metadata.json" || EXCLUDED_FOLDERS.contains(&part))
        || name == "metadata.json"
}

fn is_license_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let (stem, extension) = upper
        .split_once('.')
        .map_or((upper.as_str(), None), |(stem, extension)| {
            (stem, Some(extension))
        });
    let plain = matches!(stem, "LICENSE" | "LICENCE" | "COPYING" | "UNLICENSE");
    let named = (stem.starts_with("LICENSE-") || stem.starts_with("LICENCE-"))
        && matches!(extension, None | Some("MD" | "TXT"));
    plain || named
}

/// Why a source archive could not be read.
#[derive(Debug)]
pub enum ArchiveError {
    Read(io::Error),
    NonUtf8Path(String),
    InvalidPath(String),
    MissingLinkTarget(String),
    UnsupportedEntry(String),
    MultipleTopFolders { first: String, second: String },
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(source) => write!(f, "cannot read archive: {source}"),
            Self::NonUtf8Path(path) => write!(f, "archive path {path:?} is not UTF-8"),
            Self::InvalidPath(path) => write!(
                f,
                "archive path {path:?} must be relative, with no empty, \".\", or \"..\" component, and no '\\' or ':'"
            ),
            Self::MissingLinkTarget(path) => write!(f, "archive symlink {path:?} has no target"),
            Self::UnsupportedEntry(path) => {
                write!(f, "archive entry {path:?} is not a file, folder or symlink")
            }
            Self::MultipleTopFolders { first, second } => write!(
                f,
                "archive has more than one top-level folder ({first:?} and {second:?})"
            ),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// Why a skill could not be taken from a repository.
#[derive(Debug, PartialEq, Eq)]
pub enum SkillError {
    MissingFolder,
    MissingFile,
    /// Empty when the skill folder itself is a symlink; otherwise the path of the
    /// first symlink under the skill folder.
    Symlink(String),
    HashMismatch(ContentHash),
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFolder => f.write_str("the skill folder does not exist"),
            Self::MissingFile => f.write_str("the skill folder has no SKILL.md"),
            Self::Symlink(path) if path.is_empty() => f.write_str("the skill folder is a symlink"),
            Self::Symlink(path) => write!(f, "the skill folder contains a symlink at {path:?}"),
            Self::HashMismatch(actual) => write!(f, "the files hash to {actual}"),
        }
    }
}

impl std::error::Error for SkillError {}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::{ArchiveError, Repository, SkillError, SkillFile};
    use crate::content_hash::ContentHash;
    use crate::lock::SkillPath;

    struct Tarball {
        builder: tar::Builder<GzEncoder<Vec<u8>>>,
    }

    impl Tarball {
        fn new() -> Self {
            Self {
                builder: tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default())),
            }
        }

        fn file(mut self, path: &str, mode: u32, contents: &[u8]) -> Self {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(mode);
            header.set_size(contents.len() as u64);
            self.builder
                .append_data(&mut header, path, contents)
                .unwrap();
            self
        }

        /// Writes `path` into the header name field. `append_data` refuses `..`
        /// before the archive exists, so a rejected path would never be parsed.
        fn named(mut self, path: &str) -> Self {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(1);
            {
                let gnu = header.as_gnu_mut().unwrap();
                let bytes = path.as_bytes();
                assert!(bytes.len() <= gnu.name.len(), "{path}");
                gnu.name[..bytes.len()].copy_from_slice(bytes);
            }
            header.set_cksum();
            self.builder.append(&header, &b"x"[..]).unwrap();
            self
        }

        fn symlink(mut self, path: &str, target: &str) -> Self {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            self.builder.append_link(&mut header, path, target).unwrap();
            self
        }

        fn finish(self) -> Vec<u8> {
            self.builder.into_inner().unwrap().finish().unwrap()
        }
    }

    fn skill<'a>(
        repository: &Repository,
        skill_path: &str,
        files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<Vec<String>, SkillError> {
        let hash = ContentHash::of_files(files);
        let skill = repository.skill(&SkillPath::parse(skill_path).unwrap(), &hash)?;
        Ok(skill.files().keys().cloned().collect())
    }

    #[test]
    fn reads_files_and_the_executable_bit() {
        let archive = Tarball::new()
            .file("repo/skills/s/SKILL.md", 0o644, b"skill")
            .file("repo/skills/s/run.sh", 0o755, b"#!/bin/sh\n")
            .finish();
        let repository = Repository::from_tar_gz(archive.as_slice()).unwrap();
        let hash =
            ContentHash::of_files([("SKILL.md", &b"skill"[..]), ("run.sh", &b"#!/bin/sh\n"[..])]);
        let skill = repository
            .skill(&SkillPath::parse("skills/s/SKILL.md").unwrap(), &hash)
            .unwrap();
        assert!(!skill.files()["SKILL.md"].executable);
        assert!(skill.files()["run.sh"].executable);
        assert_eq!(skill.files()["run.sh"].contents, b"#!/bin/sh\n");
    }

    #[test]
    fn leaves_out_metadata_git_and_python_caches() {
        let mut repository = Repository::new();
        for path in [
            "s/SKILL.md",
            "s/metadata.json",
            "s/docs/metadata.json",
            "s/.git/config",
            "s/__pycache__/a.pyc",
            "s/lib/__pypackages__/x",
            "s/metadata.json.bak",
            "s/.github/workflow.yml",
        ] {
            repository.add_file(path, SkillFile::new(*b"x")).unwrap();
        }
        assert_eq!(
            skill(
                &repository,
                "s/SKILL.md",
                [
                    ("SKILL.md", &b"x"[..]),
                    ("metadata.json.bak", &b"x"[..]),
                    (".github/workflow.yml", &b"x"[..]),
                ]
            )
            .unwrap(),
            [".github/workflow.yml", "SKILL.md", "metadata.json.bak"]
        );
    }

    #[test]
    fn a_symlink_in_the_skill_folder_is_an_error() {
        let archive = Tarball::new()
            .file("repo/s/SKILL.md", 0o644, b"skill")
            .symlink("repo/s/guide.md", "SKILL.md")
            .finish();
        let repository = Repository::from_tar_gz(archive.as_slice()).unwrap();
        let hash = ContentHash::of_files([("SKILL.md", &b"skill"[..])]);
        assert_eq!(
            repository.skill(&SkillPath::parse("s/SKILL.md").unwrap(), &hash),
            Err(SkillError::Symlink("guide.md".to_owned()))
        );
    }

    #[test]
    fn a_symlink_outside_the_skill_folder_is_not_part_of_the_skill() {
        let mut repository = Repository::new();
        repository
            .add_file("README.md", SkillFile::new(*b"readme"))
            .unwrap();
        repository.add_symlink("README-link").unwrap();
        repository
            .add_file("s/SKILL.md", SkillFile::new(*b"skill"))
            .unwrap();
        assert_eq!(
            skill(&repository, "s/SKILL.md", [("SKILL.md", &b"skill"[..])]).unwrap(),
            ["SKILL.md"]
        );
    }

    #[test]
    fn the_skill_folder_itself_being_a_symlink_is_an_error() {
        let mut repository = Repository::new();
        repository.add_symlink("s").unwrap();
        let hash = ContentHash::of_files(std::iter::empty());
        assert_eq!(
            repository.skill(&SkillPath::parse("s/SKILL.md").unwrap(), &hash),
            Err(SkillError::Symlink(String::new()))
        );
    }

    #[test]
    fn reports_a_missing_folder_and_a_missing_skill_file() {
        let mut repository = Repository::new();
        repository
            .add_file("skills/pdf/README.md", SkillFile::new(*b"x"))
            .unwrap();
        let hash = ContentHash::of_files(std::iter::empty());
        let path = |raw| SkillPath::parse(raw).unwrap();
        assert_eq!(
            repository.skill(&path("skills/docx/SKILL.md"), &hash),
            Err(SkillError::MissingFolder)
        );
        assert_eq!(
            repository.skill(&path("skills/pdf/SKILL.md"), &hash),
            Err(SkillError::MissingFile)
        );
    }

    #[test]
    fn a_root_skill_takes_the_whole_repository_except_symlinks() {
        let mut repository = Repository::new();
        repository
            .add_file("SKILL.md", SkillFile::new(*b"skill"))
            .unwrap();
        repository
            .add_file("LICENSE", SkillFile::new(*b"mit"))
            .unwrap();
        repository
            .add_file("lib/a.md", SkillFile::new(*b"a"))
            .unwrap();
        assert_eq!(
            skill(
                &repository,
                "SKILL.md",
                [
                    ("SKILL.md", &b"skill"[..]),
                    ("LICENSE", &b"mit"[..]),
                    ("lib/a.md", &b"a"[..]),
                ]
            )
            .unwrap(),
            ["LICENSE", "SKILL.md", "lib/a.md"]
        );
    }

    #[test]
    fn license_files_are_regular_files_at_the_root() {
        let mut repository = Repository::new();
        for name in [
            "LICENSE-MIT",
            "LICENSE-APACHE.txt",
            "COPYING.LESSER",
            "license.md",
            "UNLICENSE",
            "license-checker.js",
            "MY_LICENSE",
            "README.md",
            "skills/s/LICENSE",
            "LICENSES/MIT.txt",
        ] {
            repository.add_file(name, SkillFile::new(*b"x")).unwrap();
        }
        repository.add_symlink("LICENCE").unwrap();
        assert_eq!(
            repository.license_files().into_keys().collect::<Vec<_>>(),
            [
                "COPYING.LESSER",
                "LICENSE-APACHE.txt",
                "LICENSE-MIT",
                "UNLICENSE",
                "license.md"
            ]
        );
    }

    #[test]
    fn hash_mismatch_returns_the_hash_of_the_files() {
        let mut repository = Repository::new();
        repository
            .add_file("s/SKILL.md", SkillFile::new(*b"skill"))
            .unwrap();
        let expected = ContentHash::of_files([("SKILL.md", &b"other"[..])]);
        let actual = ContentHash::of_files([("SKILL.md", &b"skill"[..])]);
        assert_eq!(
            repository.skill(&SkillPath::parse("s/SKILL.md").unwrap(), &expected),
            Err(SkillError::HashMismatch(actual))
        );
    }

    #[test]
    fn rejects_paths_that_are_not_plain_relative() {
        let mut repository = Repository::new();
        for path in ["", "a//b", "a/../b", "./a", "a\\b", "a:b", "foo/bar:baz"] {
            assert!(
                matches!(
                    repository.add_file(path, SkillFile::new(*b"x")),
                    Err(ArchiveError::InvalidPath(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_archive_entry_paths_that_are_not_plain_relative() {
        for path in [
            "repo/../x",
            "repo/a/../b",
            "repo/./a",
            "repo/a//b",
            "repo/a\\b",
            "repo/a:b",
            "repo/foo/bar:baz",
        ] {
            let archive = Tarball::new().named(path).finish();
            assert!(
                matches!(
                    Repository::from_tar_gz(archive.as_slice()),
                    Err(ArchiveError::InvalidPath(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_an_archive_with_several_top_folders() {
        let archive = Tarball::new()
            .file("a/SKILL.md", 0o644, b"a")
            .file("b/SKILL.md", 0o644, b"b")
            .finish();
        assert!(matches!(
            Repository::from_tar_gz(archive.as_slice()),
            Err(ArchiveError::MultipleTopFolders { .. })
        ));
    }

    #[test]
    fn rejects_data_that_is_not_a_gzipped_tarball() {
        assert!(matches!(
            Repository::from_tar_gz(&b"<html>not found</html>"[..]),
            Err(ArchiveError::Read(_))
        ));
    }
}
