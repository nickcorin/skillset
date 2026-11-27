use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use skillset::content_hash::ContentHash;
use skillset::install::{DirectoryProblem, InstallError, InstallFolder, Missing};
use skillset::lock::{CommitSha, GitHubRepo, Lockfile};
use skillset::repository::{ArchiveError, Repository};
use skillset::skill::SkillFile;
use skillset::source::Source;
use tempfile::TempDir;

const REPO: &str = "acme/agent-skills";
const FIRST: &str = "1111111111111111111111111111111111111111";
const SECOND: &str = "2222222222222222222222222222222222222222";
const MIT_LICENSE: &[u8] = b"MIT License\n\nCopyright (c) 2026 Acme\n";

#[derive(Clone)]
struct SourceCommit {
    repo: &'static str,
    sha: &'static str,
    files: BTreeMap<String, SkillFile>,
}

impl SourceCommit {
    fn demo(sha: &'static str) -> Self {
        Self {
            repo: REPO,
            sha,
            files: BTreeMap::new(),
        }
        .with_file("LICENSE", MIT_LICENSE)
        .with_file("README.md", b"# Acme skills\n")
        .with_file("skills/pdf/SKILL.md", b"---\nname: pdf\n---\n# PDF\n")
        .with_executable("skills/pdf/scripts/run.sh", b"#!/bin/sh\necho run\n")
        .with_file("skills/pdf/assets/blob.bin", b"\0\x01\r\n\xff")
        .with_file("skills/pdf/docs/crlf.md", b"line one\r\nline two\r\n")
        .with_file("skills/notes/SKILL.md", b"---\nname: notes\n---\n# Notes\n")
    }

    fn with_file(mut self, path: &str, contents: &[u8]) -> Self {
        self.files.insert(path.to_owned(), SkillFile::new(contents));
        self
    }

    fn with_executable(mut self, path: &str, contents: &[u8]) -> Self {
        self.files
            .insert(path.to_owned(), SkillFile::executable(contents));
        self
    }

    fn without(mut self, path: &str) -> Self {
        self.files.remove(path);
        self
    }

    fn repository(&self) -> Repository {
        let mut repository = Repository::new();
        for (path, file) in &self.files {
            repository.add_file(path, file.clone()).unwrap();
        }
        repository
    }

    fn skill_files(&self, skill: &str) -> BTreeMap<String, SkillFile> {
        let prefix = format!("skills/{skill}/");
        self.files
            .iter()
            .filter_map(|(path, file)| {
                let relative = path.strip_prefix(&prefix)?;
                Some((
                    relative.to_owned(),
                    SkillFile {
                        contents: file.contents.clone(),
                        executable: file.executable && cfg!(unix),
                    },
                ))
            })
            .collect()
    }

    fn lock_entry(&self, skill: &str) -> Value {
        let files = self.skill_files(skill);
        let hash = ContentHash::of_files(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.contents.as_slice())),
        );
        json!({
            "source": self.repo,
            "sourceType": "github",
            "commit": self.sha,
            "skillPath": format!("skills/{skill}/SKILL.md"),
            "computedHash": hash.as_str(),
        })
    }
}

struct Lock {
    json: Value,
}

impl Lock {
    fn new() -> Self {
        Self {
            json: json!({ "version": 2, "skills": {} }),
        }
    }

    fn skill(self, name: &str, source: &SourceCommit) -> Self {
        let entry = source.lock_entry(name);
        self.entry(name, entry)
    }

    fn entry(mut self, name: &str, entry: Value) -> Self {
        self.json["skills"][name] = entry;
        self
    }

    fn install_dir(mut self, dir: &str) -> Self {
        self.json["skillset"] = json!({ "installDir": dir });
        self
    }
}

struct Memory {
    repositories: BTreeMap<(String, String), Repository>,
    fetches: Cell<usize>,
}

impl Memory {
    fn new() -> Self {
        Self {
            repositories: BTreeMap::new(),
            fetches: Cell::new(0),
        }
    }

    fn serving(self, source: &SourceCommit) -> Self {
        self.serving_repository(source.repo, source.sha, source.repository())
    }

    fn serving_repository(mut self, repo: &str, commit: &str, repository: Repository) -> Self {
        self.repositories
            .insert((repo.to_owned(), commit.to_owned()), repository);
        self
    }
}

#[derive(Debug)]
struct Unavailable {
    repo: String,
    commit: String,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at commit {} is not available",
            self.repo, self.commit
        )
    }
}

impl std::error::Error for Unavailable {}

impl Source for Memory {
    type Error = Unavailable;

    fn repository(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<Repository, Unavailable> {
        self.fetches.set(self.fetches.get() + 1);
        self.repositories
            .get(&(repo.to_string(), commit.to_string()))
            .cloned()
            .ok_or(Unavailable {
                repo: repo.to_string(),
                commit: commit.to_string(),
            })
    }
}

struct Project {
    root: TempDir,
    dir: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
enum Node {
    Directory,
    File {
        contents: Vec<u8>,
        modified: SystemTime,
    },
    Symlink(PathBuf),
}

impl Project {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("project");
        fs::create_dir(&dir).unwrap();
        Self { root, dir }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn outside(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn lock_path(&self) -> PathBuf {
        self.path("skills-lock.json")
    }

    fn write_lock(&self, lock: &Lock) {
        fs::write(self.lock_path(), format!("{:#}\n", lock.json)).unwrap();
    }

    fn install(&self, source: &Memory) -> Result<(), InstallError> {
        InstallFolder::install(&self.lock_path(), None, source)
    }

    fn install_into(&self, source: &Memory, dir: &str) -> Result<(), InstallError> {
        InstallFolder::install(&self.lock_path(), Some(Path::new(dir)), source)
    }

    fn files(&self, relative: &str) -> BTreeMap<String, SkillFile> {
        files_at(&self.path(relative))
    }

    fn entries(&self, relative: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path(relative))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn state(&self) -> BTreeMap<PathBuf, Node> {
        let mut state = BTreeMap::new();
        let mut pending = vec![self.dir.clone()];
        while let Some(folder) = pending.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(&self.dir).unwrap().to_path_buf();
                let file_type = entry.file_type().unwrap();
                if file_type.is_symlink() {
                    state.insert(relative, Node::Symlink(fs::read_link(&path).unwrap()));
                } else if file_type.is_dir() {
                    state.insert(relative, Node::Directory);
                    pending.push(path);
                } else {
                    let modified = entry.metadata().unwrap().modified().unwrap();
                    state.insert(
                        relative,
                        Node::File {
                            contents: fs::read(&path).unwrap(),
                            modified,
                        },
                    );
                }
            }
        }
        state
    }

    fn age_files(&self) {
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for (relative, node) in self.state() {
            if matches!(node, Node::File { .. }) {
                let file = fs::File::options()
                    .write(true)
                    .open(self.dir.join(relative))
                    .unwrap();
                file.set_modified(past).unwrap();
            }
        }
    }
}

fn files_at(folder: &Path) -> BTreeMap<String, SkillFile> {
    let mut files = BTreeMap::new();
    let mut pending = vec![(folder.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let path = format!("{prefix}{name}");
            let file_type = entry.file_type().unwrap();
            assert!(
                !file_type.is_symlink(),
                "{} is a symlink",
                entry.path().display()
            );
            if file_type.is_dir() {
                pending.push((entry.path(), format!("{path}/")));
                continue;
            }
            #[cfg(unix)]
            let executable = entry.metadata().unwrap().permissions().mode() & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = false;
            files.insert(
                path,
                SkillFile {
                    contents: fs::read(entry.path()).unwrap(),
                    executable,
                },
            );
        }
    }
    files
}

fn link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link).unwrap();
        } else {
            std::os::windows::fs::symlink_file(target, link).unwrap();
        }
    }
}

fn absent(path: &Path) -> bool {
    matches!(
        fs::symlink_metadata(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound
    )
}

#[test]
fn installs_files_byte_for_byte_into_the_default_folder() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));

    project.install(&memory).unwrap();

    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
    assert_eq!(
        project.files(".agents/skills/notes"),
        source.skill_files("notes")
    );
    assert_eq!(
        project.entries(".agents/skills"),
        [".licenses", "notes", "pdf"]
    );
}

#[test]
fn chooses_dir_then_the_lock_install_dir_then_the_default() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);

    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&memory).unwrap();
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );

    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("vendor/skills")
            .skill("pdf", &source),
    );
    project.install(&memory).unwrap();
    assert_eq!(
        project.files("vendor/skills/pdf"),
        source.skill_files("pdf")
    );
    assert!(absent(&project.path(".agents")));

    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("vendor/skills")
            .skill("pdf", &source),
    );
    project.install_into(&memory, "chosen").unwrap();
    assert_eq!(project.files("chosen/pdf"), source.skill_files("pdf"));
    assert!(absent(&project.path("vendor")));
    assert!(absent(&project.path(".agents")));
}

#[test]
fn installs_the_repository_license_beside_each_skill() {
    let source = SourceCommit::demo(FIRST).with_file("COPYING", b"GPL text\n");
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));

    project.install(&memory).unwrap();

    let licenses = BTreeMap::from([
        ("COPYING".to_owned(), SkillFile::new(*b"GPL text\n")),
        ("LICENSE".to_owned(), SkillFile::new(MIT_LICENSE)),
    ]);
    assert_eq!(project.files(".agents/skills/.licenses/pdf"), licenses);
    assert_eq!(project.files(".agents/skills/.licenses/notes"), licenses);
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
}

#[test]
fn a_repository_without_a_license_file_is_an_error() {
    let source = SourceCommit::demo(FIRST).without("LICENSE");
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));

    let err = project.install(&memory).unwrap_err();

    assert!(
        matches!(err, InstallError::NoLicense { ref skill, .. } if skill == "pdf"),
        "{err}"
    );
    assert!(absent(&project.path(".agents")));
}

#[test]
fn a_hash_mismatch_leaves_the_install_folder_unchanged() {
    let first = SourceCommit::demo(FIRST);
    let second = SourceCommit::demo(SECOND)
        .with_file("skills/pdf/SKILL.md", b"# PDF, second commit\n")
        .with_file("skills/notes/SKILL.md", b"# Notes, second commit\n");
    let memory = Memory::new().serving(&first).serving(&second);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &first).skill("notes", &first));
    project.install(&memory).unwrap();

    let mut tampered = second.lock_entry("pdf");
    tampered["computedHash"] = first.lock_entry("pdf")["computedHash"].clone();
    project.write_lock(&Lock::new().skill("notes", &second).entry("pdf", tampered));
    let before = project.state();

    let err = project.install(&memory).unwrap_err();

    assert!(
        matches!(err, InstallError::HashMismatch { ref skill, .. } if skill == "pdf"),
        "{err}"
    );
    assert_eq!(project.state(), before);

    let project = Project::new();
    let mut tampered = first.lock_entry("pdf");
    tampered["computedHash"] = json!("0".repeat(64));
    project.write_lock(&Lock::new().skill("notes", &first).entry("pdf", tampered));
    let err = project.install(&memory).unwrap_err();
    assert!(matches!(err, InstallError::HashMismatch { .. }), "{err}");
    assert!(absent(&project.path(".agents")));
}

#[test]
fn a_second_run_with_an_unchanged_lock_writes_nothing() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));
    project.install(&memory).unwrap();
    project.age_files();
    let before = project.state();

    project.install(&memory).unwrap();

    assert_eq!(project.state(), before);
}

#[test]
fn removes_a_dropped_skill_and_anything_else_in_the_folder() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));
    project.install(&memory).unwrap();
    fs::create_dir(project.path(".agents/skills/my-own")).unwrap();
    fs::write(project.path(".agents/skills/my-own/SKILL.md"), "# mine\n").unwrap();
    fs::write(
        project.path(".agents/skills/.skillset-manifest.json"),
        "{}\n",
    )
    .unwrap();
    project.age_files();
    let pdf_before = project.files(".agents/skills/pdf");

    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&memory).unwrap();

    assert_eq!(project.entries(".agents/skills"), [".licenses", "pdf"]);
    assert_eq!(project.entries(".agents/skills/.licenses"), ["pdf"]);
    assert_eq!(project.files(".agents/skills/pdf"), pdf_before);
    let kept = project.state();
    let modified = match &kept[Path::new(".agents/skills/pdf/SKILL.md")] {
        Node::File { modified, .. } => *modified,
        other => panic!("expected a file, got {other:?}"),
    };
    assert_eq!(modified, UNIX_EPOCH + Duration::from_secs(1_000_000_000));
    assert!(absent(&project.path(".agents/skills/notes")));
    assert!(absent(&project.path(".agents/skills/my-own")));
    assert!(absent(
        &project.path(".agents/skills/.skillset-manifest.json")
    ));
}

#[test]
fn removes_a_symlink_in_the_install_folder_without_following_it() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&memory).unwrap();

    let outside = project.outside("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret\n").unwrap();
    link(&outside, &project.path(".agents/skills/stray"));
    link(
        &outside.join("secret.txt"),
        &project.path(".agents/skills/pdf/secret.txt"),
    );

    project.install(&memory).unwrap();

    assert!(absent(&project.path(".agents/skills/stray")));
    assert!(absent(&project.path(".agents/skills/pdf/secret.txt")));
    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"secret\n");
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
    assert!(outside.join("secret.txt").is_file());

    fs::remove_dir_all(project.path(".agents/skills")).unwrap();
    link(&outside, &project.path(".agents/skills"));
    project.install(&memory).unwrap();
    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"secret\n");
    assert!(absent(&outside.join("pdf")));
    assert!(
        !fs::symlink_metadata(project.path(".agents/skills"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
}

#[test]
fn replaces_a_skill_whose_installed_files_no_longer_match() {
    let first = SourceCommit::demo(FIRST);
    let second = SourceCommit::demo(SECOND)
        .without("skills/pdf/docs/crlf.md")
        .with_file("skills/pdf/docs/added.md", b"added\n")
        .with_file("LICENSE", b"Apache License\n");
    let memory = Memory::new().serving(&first).serving(&second);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &first).skill("notes", &first));
    project.install(&memory).unwrap();

    project.write_lock(&Lock::new().skill("pdf", &second).skill("notes", &first));
    project.install(&memory).unwrap();

    assert_eq!(
        project.files(".agents/skills/pdf"),
        second.skill_files("pdf")
    );
    assert_eq!(
        project.files(".agents/skills/.licenses/pdf"),
        BTreeMap::from([("LICENSE".to_owned(), SkillFile::new(*b"Apache License\n"))])
    );
    assert_eq!(
        project.files(".agents/skills/notes"),
        first.skill_files("notes")
    );

    fs::write(project.path(".agents/skills/pdf/SKILL.md"), "edited\n").unwrap();
    fs::remove_file(project.path(".agents/skills/pdf/docs/added.md")).unwrap();
    project.install(&memory).unwrap();
    assert_eq!(
        project.files(".agents/skills/pdf"),
        second.skill_files("pdf")
    );
}

#[test]
fn installs_a_lock_written_by_npx_skills() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills");
    let tarball = fs::read(fixtures.join("demo-skills.tar.gz")).unwrap();
    let archive = Repository::from_tar_gz(tarball.as_slice()).unwrap();
    let mut repository = Repository::new();
    for (name, file) in archive.license_files() {
        repository.add_file(&name, file).unwrap();
    }
    add_tree(
        &mut repository,
        "skills/pdf-tools",
        &fixtures.join("installed-by-npx/pdf-tools"),
    );
    add_tree(
        &mut repository,
        "skills/release-notes",
        &fixtures.join("installed-by-npx/release-notes"),
    );
    let memory = Memory::new().serving_repository(
        "skillset-fixture-owner/demo-skills",
        "b45dbefda7e1098d90f140b803059d6ecbc367dc",
        repository,
    );
    let project = Project::new();
    fs::copy(fixtures.join("skills-lock.json"), project.lock_path()).unwrap();

    project.install(&memory).unwrap();

    assert_eq!(
        project.files(".agents/skills/pdf-tools"),
        files_at(&fixtures.join("installed-by-npx/pdf-tools"))
    );
    assert_eq!(
        project.files(".agents/skills/release-notes"),
        files_at(&fixtures.join("installed-by-npx/release-notes"))
    );
    let licenses = archive.license_files();
    for skill in ["pdf-tools", "release-notes"] {
        assert_eq!(
            project.files(&format!(".agents/skills/.licenses/{skill}")),
            licenses
        );
    }
}

#[test]
fn reads_a_lock_written_by_npx_skills_for_github_repositories() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-github/skills-lock.json");

    let lock = Lockfile::read(&path).unwrap();

    let entries: Vec<_> = lock
        .entries
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.folder_name.as_str(),
                entry.source.to_string(),
                entry.commit.as_str(),
                entry.skill_path.as_str(),
                entry.computed_hash.as_str(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (
                "find-skills",
                "find-skills",
                "vercel-labs/skills".to_owned(),
                "e7fd0e9e52f029fe87729d8185cb8caf7049f678",
                "skills/find-skills/SKILL.md",
                "7bc223e2aafbdbf6e50ea243ba6ef67fe432bb069ab439fea0390795dac0bf7f",
            ),
            (
                "web-design-guidelines",
                "web-design-guidelines",
                "vercel-labs/agent-skills".to_owned(),
                "5a37a6329fbad3666c3207392a6a7c0110e92efb",
                "skills/web-design-guidelines/SKILL.md",
                "5994e4cfd86144903d2ba750f862b9e36af15e3297aac983679e347b99f6cb71",
            ),
        ]
    );
    assert_eq!(lock.install_dir, None);
}

#[test]
fn a_symlink_in_the_source_skill_is_an_error() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills");
    let tarball = fs::read(fixtures.join("demo-skills.tar.gz")).unwrap();
    let repository = Repository::from_tar_gz(tarball.as_slice()).unwrap();
    let memory = Memory::new().serving_repository(
        "skillset-fixture-owner/demo-skills",
        "b45dbefda7e1098d90f140b803059d6ecbc367dc",
        repository,
    );
    let project = Project::new();
    fs::copy(fixtures.join("skills-lock.json"), project.lock_path()).unwrap();

    let err = project.install(&memory).unwrap_err();

    assert!(
        matches!(err, InstallError::Symlink { ref path, .. } if path == "broken-link.md"),
        "{err}"
    );
    assert!(absent(&project.path(".agents")));
}

#[test]
fn rejects_archive_paths_that_are_not_plain_relative() {
    for path in [
        "repo/../x",
        "repo/a/../b",
        "repo/./a",
        "repo/a//b",
        "repo/a\\b",
        "repo/a:b",
        "repo/foo/bar:baz",
    ] {
        let archive = gzip_tar(path);
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
fn rejects_an_install_dir_that_leaves_the_lock_folder() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);

    let project = Project::new();
    project.write_lock(&Lock::new().install_dir("../out").skill("pdf", &source));
    let err = project.install(&memory).unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                which: "skillset.installDir",
                problem: DirectoryProblem::Outside,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&project.outside("out")));
    assert!(project.lock_path().is_file());

    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    let err = project.install_into(&memory, "../out").unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                which: "--dir",
                problem: DirectoryProblem::Outside,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&project.outside("out")));
    assert_eq!(memory.fetches.get(), 0);

    let project = Project::new();
    let absolute = project.path("abs-target");
    project.write_lock(&Lock::new().skill("pdf", &source));
    let err = project
        .install_into(&memory, absolute.to_str().unwrap())
        .unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                problem: DirectoryProblem::Absolute,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&absolute));

    let project = Project::new();
    let absolute = project.path("abs-from-lock");
    project.write_lock(
        &Lock::new()
            .install_dir(absolute.to_str().unwrap())
            .skill("pdf", &source),
    );
    let err = project.install(&memory).unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                which: "skillset.installDir",
                problem: DirectoryProblem::Absolute,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&absolute));

    let project = Project::new();
    project.write_lock(&Lock::new().install_dir(".").skill("pdf", &source));
    let err = project.install(&memory).unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                problem: DirectoryProblem::Outside,
                ..
            }
        ),
        "{err}"
    );
    assert!(project.lock_path().is_file());
    assert!(absent(&project.path("pdf")));

    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("skills/../../etc")
            .skill("pdf", &source),
    );
    let err = project.install(&memory).unwrap_err();
    assert!(
        matches!(
            err,
            InstallError::Directory {
                problem: DirectoryProblem::Outside,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&project.outside("etc")));

    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("vendor/../skills")
            .skill("pdf", &source),
    );
    project.install(&memory).unwrap();
    assert_eq!(project.files("skills/pdf"), source.skill_files("pdf"));
    assert!(absent(&project.path("vendor")));
}

#[test]
fn a_missing_skill_or_a_failed_fetch_writes_nothing() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    let mut entry = source.lock_entry("pdf");
    entry["skillPath"] = json!("skills/absent/SKILL.md");
    project.write_lock(&Lock::new().entry("absent", entry));

    let err = project.install(&memory).unwrap_err();

    assert!(
        matches!(
            err,
            InstallError::Missing {
                missing: Missing::Folder,
                ..
            }
        ),
        "{err}"
    );
    assert!(absent(&project.path(".agents")));

    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&memory).unwrap();
    let unpublished = SourceCommit::demo(SECOND);
    project.write_lock(&Lock::new().skill("pdf", &unpublished));
    let before = project.state();
    let err = project.install(&memory).unwrap_err();
    assert!(matches!(err, InstallError::Fetch { .. }), "{err}");
    assert_eq!(project.state(), before);
}

#[test]
fn an_unsupported_lock_entry_fails_before_fetching() {
    let source = SourceCommit::demo(FIRST);
    let memory = Memory::new().serving(&source);
    let project = Project::new();
    let local = json!({
        "source": "./my-skills",
        "sourceType": "local",
        "computedHash": "0".repeat(64)
    });
    project.write_lock(&Lock::new().skill("pdf", &source).entry("mine", local));

    let err = project.install(&memory).unwrap_err();

    assert!(matches!(err, InstallError::Lock(_)), "{err}");
    assert_eq!(memory.fetches.get(), 0);
    assert!(absent(&project.path(".agents")));
}

fn add_tree(repository: &mut Repository, prefix: &str, directory: &Path) {
    let mut pending = vec![(directory.to_path_buf(), prefix.to_owned())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let path = format!("{prefix}/{name}");
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                pending.push((entry.path(), path));
            } else if file_type.is_file() {
                #[cfg(unix)]
                let executable = entry.metadata().unwrap().permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let executable = false;
                let contents = fs::read(entry.path()).unwrap();
                let file = if executable {
                    SkillFile::executable(contents)
                } else {
                    SkillFile::new(contents)
                };
                repository.add_file(&path, file).unwrap();
            } else {
                panic!("{} is not a regular file", entry.path().display());
            }
        }
    }
}

fn gzip_tar(path: &str) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(1);
    {
        let gnu = header.as_gnu_mut().unwrap();
        let bytes = path.as_bytes();
        gnu.name[..bytes.len()].copy_from_slice(bytes);
    }
    header.set_cksum();
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append(&header, &b"x"[..]).unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}
