//! A repository's files at one commit, and the skill folders read from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};

use flate2::read::GzDecoder;

use crate::content_hash::ContentHash;
use crate::lock::SkillPath;

/// Names `npx skills` leaves out of a skill as files or as folders.
const EXCLUDED_NAMES: [&str; 1] = ["metadata.json"];
/// Names `npx skills` additionally leaves out of a skill as folders.
const EXCLUDED_FOLDER_NAMES: [&str; 3] = [".git", "__pycache__", "__pypackages__"];
/// Symlink hops allowed while resolving one path, matching Linux's `ELOOP` limit.
const MAX_SYMLINK_HOPS: usize = 40;

/// One file's bytes and whether Git records it as executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    pub contents: Vec<u8>,
    pub executable: bool,
}

impl SkillFile {
    pub fn new(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
            executable: false,
        }
    }

    pub fn executable(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
            executable: true,
        }
    }
}

/// The files a skill installs, keyed by `/`-separated path relative to its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedSkill {
    pub files: BTreeMap<String, SkillFile>,
}

impl FetchedSkill {
    pub fn content_hash(&self) -> ContentHash {
        ContentHash::of_files(
            self.files
                .iter()
                .map(|(path, file)| (path.as_str(), file.contents.as_slice())),
        )
    }
}

#[derive(Clone, Debug)]
enum Node {
    File(SkillFile),
    Symlink(String),
}

enum Kind<'a> {
    Folder,
    File(&'a SkillFile),
    Symlink(&'a str),
}

/// Every file and symlink of a repository at one commit, keyed by `/`-separated
/// repository-relative path. Folders are implied by the paths beneath them.
#[derive(Clone, Debug, Default)]
pub struct RepoSnapshot {
    nodes: BTreeMap<String, Node>,
}

impl RepoSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_file(&mut self, path: &str, file: SkillFile) -> Result<(), ArchiveError> {
        self.insert(path, Node::File(file))
    }

    pub fn insert_symlink(&mut self, path: &str, target: &str) -> Result<(), ArchiveError> {
        self.insert(path, Node::Symlink(target.to_owned()))
    }

    /// Reads a gzipped tarball whose entries all sit under one top-level folder,
    /// which is how GitHub (and `git archive --prefix`) lay out a commit.
    pub fn from_tar_gz(reader: impl Read) -> Result<Self, ArchiveError> {
        let mut archive = tar::Archive::new(GzDecoder::new(reader));
        let mut snapshot = Self::new();
        let mut top_folder: Option<String> = None;
        for entry in archive.entries().map_err(ArchiveError::Read)? {
            let mut entry = entry.map_err(ArchiveError::Read)?;
            let entry_type = entry.header().entry_type();
            if entry_type.is_pax_global_extensions() {
                continue;
            }
            let path = String::from_utf8(entry.path_bytes().into_owned()).map_err(|err| {
                ArchiveError::NonUtf8Path(String::from_utf8_lossy(err.as_bytes()).into_owned())
            })?;
            let path = path.trim_end_matches('/');
            let (top, inner) = path.split_once('/').unwrap_or((path, ""));
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

            if entry_type.is_symlink() {
                let target = entry
                    .link_name_bytes()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .ok_or_else(|| ArchiveError::MissingLinkTarget(inner.to_owned()))?;
                snapshot.insert_symlink(inner, &target)?;
            } else if entry_type.is_file() || entry_type.is_contiguous() {
                let mode = entry.header().mode().map_err(ArchiveError::Read)?;
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .map_err(ArchiveError::Read)?;
                let file = SkillFile {
                    contents,
                    executable: mode & 0o111 != 0,
                };
                snapshot.insert_file(inner, file)?;
            } else {
                return Err(ArchiveError::UnsupportedEntry(inner.to_owned()));
            }
        }
        Ok(snapshot)
    }

    /// The files `npx skills` installs for the skill whose `SKILL.md` is at
    /// `skill_path`: everything in its folder except excluded names, with symlinks
    /// replaced by what they point to and links that are broken, loop, or leave the
    /// folder left out.
    pub fn skill_files(&self, skill_path: &SkillPath) -> Result<FetchedSkill, MissingSkill> {
        let root = self
            .resolve(skill_path.folder())
            .filter(|root| matches!(self.kind(root), Some(Kind::Folder)))
            .ok_or(MissingSkill::Folder)?;
        let mut files = BTreeMap::new();
        self.collect_files(&root, "", &root, &mut Vec::new(), &mut files);
        if !files.contains_key(skill_path.file_name()) {
            return Err(MissingSkill::SkillFile);
        }
        Ok(FetchedSkill { files })
    }

    /// Files at the repository root named like a license, in any letter case:
    /// `LICENSE`, `LICENCE`, `COPYING` or `UNLICENSE` with any extension, or
    /// `LICENSE-<name>` (such as `LICENSE-MIT`) with no extension, `.md` or `.txt`.
    pub fn license_files(&self) -> BTreeMap<String, SkillFile> {
        self.children("")
            .into_iter()
            .filter(|name| {
                let upper = name.to_ascii_uppercase();
                let (stem, extension) = upper
                    .split_once('.')
                    .map_or((upper.as_str(), None), |(stem, extension)| {
                        (stem, Some(extension))
                    });
                let is_plain_name = matches!(stem, "LICENSE" | "LICENCE" | "COPYING" | "UNLICENSE");
                let is_named_variant = (stem.starts_with("LICENSE-")
                    || stem.starts_with("LICENCE-"))
                    && matches!(extension, None | Some("MD" | "TXT"));
                is_plain_name || is_named_variant
            })
            .filter_map(|name| match self.kind(&self.resolve(&name)?) {
                Some(Kind::File(file)) => Some((name, file.clone())),
                Some(Kind::Folder | Kind::Symlink(_)) | None => None,
            })
            .collect()
    }

    fn insert(&mut self, path: &str, node: Node) -> Result<(), ArchiveError> {
        let is_plain = path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
        if !is_plain {
            return Err(ArchiveError::InvalidPath(path.to_owned()));
        }
        self.nodes.insert(path.to_owned(), node);
        Ok(())
    }

    fn collect_files(
        &self,
        folder: &str,
        prefix: &str,
        root: &str,
        ancestors: &mut Vec<String>,
        files: &mut BTreeMap<String, SkillFile>,
    ) {
        if ancestors.iter().any(|ancestor| ancestor == folder) {
            return;
        }
        ancestors.push(folder.to_owned());
        for name in self.children(folder) {
            let child = if folder.is_empty() {
                name.clone()
            } else {
                format!("{folder}/{name}")
            };
            let (real_path, kind) = match self.kind(&child) {
                Some(Kind::Symlink(_)) => {
                    let inside_root = self.resolve(&child).filter(|target| {
                        root.is_empty()
                            || target == root
                            || target
                                .strip_prefix(root)
                                .is_some_and(|rest| rest.starts_with('/'))
                    });
                    let Some(target) = inside_root else {
                        continue;
                    };
                    let kind = self.kind(&target);
                    (target, kind)
                }
                kind => (child, kind),
            };
            let name = name.as_str();
            match kind {
                Some(Kind::Folder) => {
                    if !EXCLUDED_NAMES.contains(&name) && !EXCLUDED_FOLDER_NAMES.contains(&name) {
                        let prefix = format!("{prefix}{name}/");
                        self.collect_files(&real_path, &prefix, root, ancestors, files);
                    }
                }
                Some(Kind::File(file)) => {
                    if !EXCLUDED_NAMES.contains(&name) {
                        files.insert(format!("{prefix}{name}"), file.clone());
                    }
                }
                Some(Kind::Symlink(_)) | None => {}
            }
        }
        ancestors.pop();
    }

    /// The path with every symlink replaced by its target, or `None` when the path
    /// does not exist, loops, or leaves the repository.
    fn resolve(&self, path: &str) -> Option<String> {
        let mut pending: Vec<String> = path.split('/').rev().map(str::to_owned).collect();
        let mut resolved: Vec<String> = Vec::new();
        let mut hops = 0;
        while let Some(part) = pending.pop() {
            match part.as_str() {
                "" | "." => {}
                ".." => {
                    resolved.pop()?;
                }
                _ => {
                    resolved.push(part);
                    match self.kind(&resolved.join("/"))? {
                        Kind::Folder => {}
                        Kind::File(_) => {
                            if pending.iter().any(|rest| !rest.is_empty() && rest != ".") {
                                return None;
                            }
                        }
                        Kind::Symlink(target) => {
                            hops += 1;
                            if hops > MAX_SYMLINK_HOPS || target.starts_with('/') {
                                return None;
                            }
                            resolved.pop();
                            pending.extend(target.split('/').rev().map(str::to_owned));
                        }
                    }
                }
            }
        }
        Some(resolved.join("/"))
    }

    fn kind(&self, path: &str) -> Option<Kind<'_>> {
        if path.is_empty() {
            return Some(Kind::Folder);
        }
        match self.nodes.get(path) {
            Some(Node::File(file)) => Some(Kind::File(file)),
            Some(Node::Symlink(target)) => Some(Kind::Symlink(target)),
            None => {
                let prefix = format!("{path}/");
                let has_children = self
                    .nodes
                    .range(prefix.clone()..)
                    .next()
                    .is_some_and(|(key, _)| key.starts_with(&prefix));
                has_children.then_some(Kind::Folder)
            }
        }
    }

    fn children(&self, folder: &str) -> BTreeSet<String> {
        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        self.nodes
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .map(|(key, _)| {
                let rest = &key[prefix.len()..];
                rest.split_once('/')
                    .map_or(rest, |(name, _)| name)
                    .to_owned()
            })
            .collect()
    }
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
            Self::InvalidPath(path) => {
                write!(f, "archive path {path:?} is not a plain relative path")
            }
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

/// Why a skill could not be read from a snapshot.
#[derive(Debug, PartialEq, Eq)]
pub enum MissingSkill {
    Folder,
    SkillFile,
}

impl fmt::Display for MissingSkill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Folder => f.write_str("the skill folder does not exist"),
            Self::SkillFile => f.write_str("the skill folder has no SKILL.md at the recorded path"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::{ArchiveError, MissingSkill, RepoSnapshot, SkillFile};
    use crate::lock::SkillPath;

    impl RepoSnapshot {
        fn with_files(files: &[(&str, &str)], symlinks: &[(&str, &str)]) -> Self {
            let mut snapshot = Self::new();
            for (path, contents) in files {
                snapshot
                    .insert_file(path, SkillFile::new(contents.as_bytes()))
                    .unwrap();
            }
            for (path, target) in symlinks {
                snapshot.insert_symlink(path, target).unwrap();
            }
            snapshot
        }

        fn skill_file_paths(&self, skill_path: &str) -> Vec<String> {
            let skill = self
                .skill_files(&SkillPath::parse(skill_path).unwrap())
                .unwrap();
            skill.files.into_keys().collect()
        }
    }

    struct TarballBuilder {
        builder: tar::Builder<GzEncoder<Vec<u8>>>,
    }

    impl TarballBuilder {
        fn new() -> Self {
            Self {
                builder: tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default())),
            }
        }

        fn folder(mut self, path: &str) -> Self {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o775);
            header.set_size(0);
            self.builder
                .append_data(&mut header, path, &[][..])
                .unwrap();
            self
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

    #[test]
    fn collects_nested_files_of_the_skill_folder_only() {
        let snapshot = RepoSnapshot::with_files(
            &[
                ("README.md", "readme"),
                ("skills/pdf/SKILL.md", "skill"),
                ("skills/pdf/scripts/run.sh", "run"),
                ("skills/pdf-other/SKILL.md", "other"),
            ],
            &[],
        );
        assert_eq!(
            snapshot.skill_file_paths("skills/pdf/SKILL.md"),
            ["SKILL.md", "scripts/run.sh"]
        );
    }

    #[test]
    fn leaves_out_excluded_names_at_any_depth() {
        let snapshot = RepoSnapshot::with_files(
            &[
                ("s/SKILL.md", "skill"),
                ("s/metadata.json", "{}"),
                ("s/docs/metadata.json", "{}"),
                ("s/.git/config", "x"),
                ("s/__pycache__/a.pyc", "x"),
                ("s/lib/__pypackages__/x", "x"),
                ("s/metadata.json.bak", "kept"),
                ("s/.github/workflow.yml", "kept"),
            ],
            &[("s/link-to-cache", "__pycache__")],
        );
        assert_eq!(
            snapshot.skill_file_paths("s/SKILL.md"),
            [
                ".github/workflow.yml",
                "SKILL.md",
                "link-to-cache/a.pyc",
                "metadata.json.bak"
            ]
        );
    }

    #[test]
    fn follows_symlinks_that_stay_inside_the_skill() {
        let snapshot = RepoSnapshot::with_files(
            &[
                ("README.md", "readme"),
                ("s/SKILL.md", "skill"),
                ("s/refs/guide.md", "guide"),
            ],
            &[
                ("s/guide-link.md", "refs/guide.md"),
                ("s/refs-link", "refs"),
                ("s/outside.md", "../README.md"),
                ("s/absolute.md", "/etc/passwd"),
                ("s/broken.md", "refs/missing.md"),
                ("s/loop-a", "loop-b"),
                ("s/loop-b", "loop-a"),
                ("s/refs/up", ".."),
            ],
        );
        let skill = snapshot
            .skill_files(&SkillPath::parse("s/SKILL.md").unwrap())
            .unwrap();
        assert_eq!(
            skill.files.keys().collect::<Vec<_>>(),
            [
                "SKILL.md",
                "guide-link.md",
                "refs-link/guide.md",
                "refs/guide.md"
            ]
        );
        assert_eq!(skill.files["guide-link.md"].contents, b"guide");
    }

    #[test]
    fn root_skill_takes_the_whole_repository() {
        let snapshot = RepoSnapshot::with_files(
            &[("SKILL.md", "skill"), ("LICENSE", "mit"), ("lib/a.md", "a")],
            &[],
        );
        assert_eq!(
            snapshot.skill_file_paths("SKILL.md"),
            ["LICENSE", "SKILL.md", "lib/a.md"]
        );
    }

    #[test]
    fn reports_missing_folder_and_missing_skill_file() {
        let snapshot = RepoSnapshot::with_files(&[("skills/pdf/README.md", "x")], &[]);
        assert_eq!(
            snapshot.skill_files(&SkillPath::parse("skills/docx/SKILL.md").unwrap()),
            Err(MissingSkill::Folder)
        );
        assert_eq!(
            snapshot.skill_files(&SkillPath::parse("skills/pdf/SKILL.md").unwrap()),
            Err(MissingSkill::SkillFile)
        );
    }

    #[test]
    fn finds_license_files_at_the_root() {
        let snapshot = RepoSnapshot::with_files(
            &[
                ("LICENSE-MIT", "mit"),
                ("LICENSE-APACHE.txt", "apache"),
                ("COPYING.LESSER", "lgpl"),
                ("license.md", "lowercase"),
                ("UNLICENSE", "public domain"),
                ("license-checker.js", "code"),
                ("MY_LICENSE", "other"),
                ("README.md", "readme"),
                ("skills/s/LICENSE", "nested"),
                ("LICENSES/MIT.txt", "reuse"),
            ],
            &[("LICENCE", "LICENSE-MIT")],
        );
        assert_eq!(
            snapshot.license_files().into_keys().collect::<Vec<_>>(),
            [
                "COPYING.LESSER",
                "LICENCE",
                "LICENSE-APACHE.txt",
                "LICENSE-MIT",
                "UNLICENSE",
                "license.md"
            ]
        );
    }

    #[test]
    fn rejects_paths_that_are_not_plain() {
        let mut snapshot = RepoSnapshot::new();
        for path in ["", "/abs", "a//b", "a/../b", "./a"] {
            assert!(matches!(
                snapshot.insert_file(path, SkillFile::new(*b"x")),
                Err(ArchiveError::InvalidPath(_))
            ));
        }
    }

    #[test]
    fn reads_github_style_tarball() {
        let archive = TarballBuilder::new()
            .folder("repo-abc/")
            .file("repo-abc/skills/s/SKILL.md", 0o664, b"skill")
            .file("repo-abc/skills/s/run.sh", 0o775, b"#!/bin/sh\n")
            .symlink("repo-abc/skills/s/link.md", "SKILL.md")
            .finish();
        let snapshot = RepoSnapshot::from_tar_gz(archive.as_slice()).unwrap();
        let skill = snapshot
            .skill_files(&SkillPath::parse("skills/s/SKILL.md").unwrap())
            .unwrap();
        let expected = BTreeMap::from([
            ("SKILL.md".to_owned(), SkillFile::new(*b"skill")),
            ("link.md".to_owned(), SkillFile::new(*b"skill")),
            ("run.sh".to_owned(), SkillFile::executable(*b"#!/bin/sh\n")),
        ]);
        assert_eq!(skill.files, expected);
    }

    #[test]
    fn rejects_tarball_with_several_top_folders() {
        let archive = TarballBuilder::new()
            .file("a/SKILL.md", 0o644, b"a")
            .file("b/SKILL.md", 0o644, b"b")
            .finish();
        assert!(matches!(
            RepoSnapshot::from_tar_gz(archive.as_slice()),
            Err(ArchiveError::MultipleTopFolders { .. })
        ));
    }

    #[test]
    fn rejects_data_that_is_not_a_gzipped_tarball() {
        assert!(matches!(
            RepoSnapshot::from_tar_gz(&b"<html>not found</html>"[..]),
            Err(ArchiveError::Read(_))
        ));
    }
}
