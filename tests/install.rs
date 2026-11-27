use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use skillset::content_hash::ContentHash;
use skillset::install::{InstallError, InstallFolder};
use skillset::repository::Repository;
use skillset::skill::SkillFile;
use skillset::source::{CommitSha, GitHubRepo, Source};
use tempfile::TempDir;

static CWD: Mutex<()> = Mutex::new(());

const OWNER: &str = "acme";
const REPO: &str = "agent-skills";
const COMMIT: &str = "1111111111111111111111111111111111111111";
const LICENSE: &[u8] = b"MIT License\n\nCopyright (c) 2026 Acme\n";
const NOTES: &[u8] = b"# Notes\n";
const PDF: &[(&str, &[u8], bool)] = &[
    ("SKILL.md", b"---\nname: pdf\n---\n# PDF\n", false),
    ("scripts/run.sh", b"#!/bin/sh\necho run\n", true),
    ("assets/blob.bin", b"\0\x01\r\n\xff", false),
    ("docs/crlf.md", b"line one\r\nline two\r\n", false),
];

struct Project {
    root: TempDir,
    dir: PathBuf,
}

struct Memory {
    repos: BTreeMap<(String, String, String), Repository>,
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
        let root = TempDir::new().expect("temp dir");
        let dir = root.path().join("project");
        fs::create_dir(&dir).expect("project dir");
        Self { root, dir }
    }

    fn write_lock(&self, install_dir: Option<&str>, skills: Value) {
        let mut value = json!({"version": 2, "skills": skills});
        if let Some(dir) = install_dir {
            value["skillset"] = json!({"installDir": dir});
        }
        fs::write(self.dir.join("skills-lock.json"), format!("{value:#}\n")).expect("write lock");
    }

    fn install(&self, source: &Memory) -> Result<(), InstallError> {
        InstallFolder::install(&self.dir.join("skills-lock.json"), None, source)
    }

    fn install_into(&self, source: &Memory, dir: &str) -> Result<(), InstallError> {
        let _guard = CWD.lock().expect("cwd lock");
        InstallFolder::install(
            &self.dir.join("skills-lock.json"),
            Some(Path::new(dir)),
            source,
        )
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn outside(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
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
                    state.insert(
                        relative,
                        Node::File {
                            contents: fs::read(&path).unwrap(),
                            modified: entry.metadata().unwrap().modified().unwrap(),
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
                fs::File::options()
                    .write(true)
                    .open(self.dir.join(relative))
                    .unwrap()
                    .set_modified(past)
                    .unwrap();
            }
        }
    }
}

impl Memory {
    fn new(repository: Repository) -> Self {
        Self::empty().serving(OWNER, REPO, COMMIT, repository)
    }

    fn empty() -> Self {
        Self {
            repos: BTreeMap::new(),
        }
    }

    fn serving(mut self, owner: &str, repo: &str, commit: &str, repository: Repository) -> Self {
        self.repos.insert(
            (owner.to_owned(), repo.to_owned(), commit.to_owned()),
            repository,
        );
        self
    }
}

impl Source for Memory {
    type Error = io::Error;

    fn repository(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<Repository, io::Error> {
        self.repos
            .get(&(
                repo.owner().to_owned(),
                repo.name().to_owned(),
                commit.as_str().to_owned(),
            ))
            .cloned()
            .ok_or_else(|| io::Error::other(format!("{repo} at {commit} is not available")))
    }
}

fn demo() -> Repository {
    let mut repository = Repository::new();
    repository
        .add_file("LICENSE", SkillFile::new(LICENSE))
        .unwrap();
    repository
        .add_file("README.md", SkillFile::new(b"# Acme skills\n"))
        .unwrap();
    for (path, bytes, executable) in PDF {
        repository
            .add_file(
                &format!("skills/pdf/{path}"),
                skill_file(bytes, *executable),
            )
            .unwrap();
    }
    repository
        .add_file("skills/pdf/metadata.json", SkillFile::new(b"{}\n"))
        .unwrap();
    repository
        .add_file("skills/pdf/.git/config", SkillFile::new(b"[core]\n"))
        .unwrap();
    repository
        .add_file("skills/notes/SKILL.md", SkillFile::new(NOTES))
        .unwrap();
    repository
}

fn skill_file(bytes: &[u8], executable: bool) -> SkillFile {
    if executable {
        SkillFile::executable(bytes)
    } else {
        SkillFile::new(bytes)
    }
}

fn installed(files: &[(&str, &[u8], bool)]) -> BTreeMap<String, SkillFile> {
    files
        .iter()
        .map(|(path, bytes, executable)| {
            (
                (*path).to_owned(),
                skill_file(bytes, *executable && cfg!(unix)),
            )
        })
        .collect()
}

fn digest(files: &[(&str, &[u8])]) -> ContentHash {
    ContentHash::of_files(files.iter().copied())
}

fn pdf_hash() -> ContentHash {
    digest(
        &PDF.iter()
            .map(|(path, bytes, _)| (*path, *bytes))
            .collect::<Vec<_>>(),
    )
}

fn notes_hash() -> ContentHash {
    digest(&[("SKILL.md", NOTES)])
}

fn skills(names: &[&str]) -> Value {
    let mut skills = serde_json::Map::new();
    for name in names {
        let hash = if *name == "pdf" {
            pdf_hash()
        } else {
            notes_hash()
        };
        skills.insert((*name).to_owned(), entry(name, &hash));
    }
    Value::Object(skills)
}

fn entry(folder: &str, hash: &ContentHash) -> Value {
    json!({
        "source": format!("{OWNER}/{REPO}"),
        "sourceType": "github",
        "commit": COMMIT,
        "skillPath": format!("skills/{folder}/SKILL.md"),
        "computedHash": hash.as_str(),
    })
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
                skill_file(&fs::read(entry.path()).unwrap(), executable),
            );
        }
    }
    files
}

fn absent(path: &Path) -> bool {
    matches!(
        fs::symlink_metadata(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound
    )
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

#[test]
fn installs_files_byte_for_byte() {
    let memory = Memory::new(demo());
    let project = Project::new();
    project.write_lock(None, skills(&["pdf", "notes"]));

    project.install(&memory).unwrap();

    assert_eq!(
        files_at(&project.path(".agents/skills/pdf")),
        installed(PDF)
    );
    assert_eq!(
        files_at(&project.path(".agents/skills/notes")),
        installed(&[("SKILL.md", NOTES, false)])
    );
    assert!(absent(&project.path(".agents/skills/pdf/metadata.json")));
    assert!(absent(&project.path(".agents/skills/pdf/.git")));
}

#[test]
fn chooses_the_flag_then_the_lock_then_the_default_folder() {
    let memory = Memory::new(demo());

    let project = Project::new();
    project.write_lock(None, skills(&["pdf"]));
    project.install(&memory).unwrap();
    assert_eq!(
        files_at(&project.path(".agents/skills/pdf")),
        installed(PDF)
    );

    let project = Project::new();
    project.write_lock(Some("vendor/skills"), skills(&["pdf"]));
    project.install(&memory).unwrap();
    assert_eq!(files_at(&project.path("vendor/skills/pdf")), installed(PDF));
    assert!(absent(&project.path(".agents")));

    let project = Project::new();
    project.write_lock(Some("vendor/skills"), skills(&["pdf"]));
    {
        let _guard = CWD.lock().expect("cwd lock");
        let previous = env::current_dir().unwrap();
        env::set_current_dir(&project.dir).unwrap();
        let result = InstallFolder::install(
            &project.dir.join("skills-lock.json"),
            Some(Path::new("chosen")),
            &memory,
        );
        env::set_current_dir(previous).unwrap();
        result.unwrap();
    }
    assert_eq!(files_at(&project.path("chosen/pdf")), installed(PDF));
    assert!(absent(&project.path("vendor")));
    assert!(absent(&project.path(".agents")));

    let project = Project::new();
    project.write_lock(Some("vendor/skills"), skills(&["pdf"]));
    let absolute = project.path("from-flag");
    project
        .install_into(&memory, absolute.to_str().unwrap())
        .unwrap();
    assert_eq!(files_at(&absolute.join("pdf")), installed(PDF));
    assert!(absent(&project.path("vendor")));
}

#[test]
fn installs_repository_licenses_beside_each_skill() {
    let memory = Memory::new(demo());
    let project = Project::new();
    project.write_lock(None, skills(&["pdf", "notes"]));

    project.install(&memory).unwrap();

    let license = BTreeMap::from([("LICENSE".to_owned(), SkillFile::new(LICENSE))]);
    assert_eq!(
        files_at(&project.path(".agents/skills/.licenses/pdf")),
        license
    );
    assert_eq!(
        files_at(&project.path(".agents/skills/.licenses/notes")),
        license
    );
    assert!(absent(&project.path(".agents/skills/pdf/LICENSE")));
}

#[test]
fn a_hash_mismatch_leaves_the_folder_untouched() {
    let memory = Memory::new(demo());
    let project = Project::new();
    let mut listed = skills(&["pdf"]);
    listed["pdf"]["computedHash"] = json!("0".repeat(64));
    project.write_lock(None, listed);

    let err = project.install(&memory).unwrap_err();

    assert!(err.to_string().contains("computedHash"), "{err}");
    assert!(absent(&project.path(".agents")));

    project.write_lock(None, skills(&["pdf"]));
    project.install(&memory).unwrap();
    project.age_files();
    let mut listed = skills(&["pdf"]);
    listed["pdf"]["computedHash"] = json!("0".repeat(64));
    project.write_lock(None, listed);
    let before = project.state();

    let err = project.install(&memory).unwrap_err();

    assert!(err.to_string().contains("computedHash"), "{err}");
    assert_eq!(project.state(), before);
}

#[test]
fn a_second_run_writes_nothing() {
    let memory = Memory::new(demo());
    let project = Project::new();
    project.write_lock(None, skills(&["pdf", "notes"]));
    project.install(&memory).unwrap();
    project.age_files();
    let before = project.state();

    project.install(&memory).unwrap();

    assert_eq!(project.state(), before);

    let crlf = project.path(".agents/skills/pdf/docs/crlf.md");
    let text = fs::read(&crlf).unwrap();
    fs::write(
        &crlf,
        text.into_iter()
            .filter(|byte| *byte != b'\r')
            .collect::<Vec<_>>(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let script = project.path(".agents/skills/pdf/scripts/run.sh");
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(permissions.mode() & !0o111);
        fs::set_permissions(&script, permissions).unwrap();
    }

    project.install(&memory).unwrap();

    assert_eq!(
        files_at(&project.path(".agents/skills/pdf")),
        installed(PDF)
    );
    project.age_files();
    let before = project.state();
    project.install(&memory).unwrap();
    assert_eq!(project.state(), before);
}

#[test]
fn a_dropped_skill_is_removed() {
    let memory = Memory::new(demo());
    let project = Project::new();
    project.write_lock(None, skills(&["pdf", "notes"]));
    project.install(&memory).unwrap();
    fs::create_dir_all(project.path(".agents/skills/stray")).unwrap();
    fs::write(project.path(".agents/skills/stray/note.txt"), b"stray\n").unwrap();
    project.age_files();
    let pdf_modified = project
        .state()
        .remove(&PathBuf::from(".agents/skills/pdf/SKILL.md"))
        .unwrap();
    project.write_lock(None, skills(&["pdf"]));

    project.install(&memory).unwrap();

    assert!(absent(&project.path(".agents/skills/notes")));
    assert!(absent(&project.path(".agents/skills/.licenses/notes")));
    assert!(absent(&project.path(".agents/skills/stray")));
    assert_eq!(
        project
            .state()
            .remove(&PathBuf::from(".agents/skills/pdf/SKILL.md"))
            .unwrap(),
        pdf_modified
    );
}

#[test]
#[ignore = "the fixture archive contains symlinked files in the skill"]
fn an_npx_written_lock_installs() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills");
    let repository =
        Repository::from_tar_gz(fs::File::open(fixtures.join("demo-skills.tar.gz")).unwrap())
            .unwrap();
    let licenses = repository.license_files();
    let memory = Memory::empty().serving(
        "skillset-fixture-owner",
        "demo-skills",
        "b45dbefda7e1098d90f140b803059d6ecbc367dc",
        repository,
    );
    let project = Project::new();
    fs::copy(
        fixtures.join("skills-lock.json"),
        project.path("skills-lock.json"),
    )
    .unwrap();

    project.install(&memory).unwrap();

    assert_eq!(
        files_at(&project.path(".agents/skills/pdf-tools")),
        files_at(&fixtures.join("installed-by-npx/pdf-tools"))
    );
    assert_eq!(
        files_at(&project.path(".agents/skills/release-notes")),
        files_at(&fixtures.join("installed-by-npx/release-notes"))
    );
    assert!(!licenses.is_empty());
    assert_eq!(
        files_at(&project.path(".agents/skills/.licenses/pdf-tools")),
        licenses
    );
}

#[test]
fn a_symlink_in_the_install_folder_is_removed_without_being_followed() {
    let memory = Memory::new(demo());
    let project = Project::new();
    project.write_lock(None, skills(&["pdf"]));
    project.install(&memory).unwrap();
    fs::write(project.path("secret.txt"), b"secret\n").unwrap();
    fs::remove_file(project.path(".agents/skills/pdf/SKILL.md")).unwrap();
    link(
        &project.path("secret.txt"),
        &project.path(".agents/skills/pdf/SKILL.md"),
    );

    project.install(&memory).unwrap();

    assert_eq!(fs::read(project.path("secret.txt")).unwrap(), b"secret\n");
    assert_eq!(
        files_at(&project.path(".agents/skills/pdf")),
        installed(PDF)
    );

    let elsewhere = project.outside("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep.txt"), b"keep\n").unwrap();
    fs::rename(
        project.path(".agents/skills"),
        project.outside("saved-skills"),
    )
    .unwrap();
    link(&elsewhere, &project.path(".agents/skills"));

    project.install(&memory).unwrap();

    assert_eq!(fs::read(elsewhere.join("keep.txt")).unwrap(), b"keep\n");
    assert!(absent(&elsewhere.join("pdf")));
    assert_eq!(
        files_at(&project.path(".agents/skills/pdf")),
        installed(PDF)
    );
    assert!(
        fs::symlink_metadata(project.path(".agents/skills"))
            .unwrap()
            .file_type()
            .is_dir()
    );
}

#[test]
fn a_symlink_in_the_source_is_an_error() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills");
    let repository =
        Repository::from_tar_gz(fs::File::open(fixtures.join("demo-skills.tar.gz")).unwrap())
            .unwrap();
    let memory = Memory::empty().serving(
        "skillset-fixture-owner",
        "demo-skills",
        "b45dbefda7e1098d90f140b803059d6ecbc367dc",
        repository,
    );
    let project = Project::new();
    fs::copy(
        fixtures.join("skills-lock.json"),
        project.path("skills-lock.json"),
    )
    .unwrap();

    let err = project.install(&memory).unwrap_err();

    assert!(err.to_string().contains("broken-link.md"), "{err}");
    assert!(absent(&project.path(".agents")));
}

#[test]
fn a_bad_archive_path_is_an_error() {
    for path in [
        "repo/../x",
        "repo/a/../b",
        "repo/./a",
        "repo/a//b",
        "repo/a\\b",
        "repo/a:b",
        "repo/foo/bar:baz",
    ] {
        let err = Repository::from_tar_gz(gzip_tar(path).as_slice()).unwrap_err();
        assert!(
            err.to_string().contains(&format!("{path:?}")),
            "{path}: {err}"
        );
    }
}

#[test]
fn an_escaping_install_path_is_an_error() {
    let memory = Memory::new(demo());

    for (install_dir, flag, needle) in [
        (Some("../out"), None, "must stay inside"),
        (None, Some("../out"), "must stay inside"),
        (Some("."), None, "must stay inside"),
        (None, Some("skills/../../etc"), "must stay inside"),
        (
            Some("/tmp/skillset-not-created"),
            None,
            "must be a relative path",
        ),
        (None, Some("/tmp/skillset-not-created"), "must stay inside"),
    ] {
        let project = Project::new();
        project.write_lock(install_dir, skills(&["pdf"]));
        let err = match flag {
            Some(dir) => project.install_into(&memory, dir).unwrap_err(),
            None => project.install(&memory).unwrap_err(),
        };
        assert!(
            err.to_string().contains(needle),
            "{flag:?} {install_dir:?}: {err}"
        );
        assert!(absent(&project.outside("out")));
        assert!(absent(&project.outside("etc")));
        assert!(absent(&project.path(".agents")));
    }

    let project = Project::new();
    project.write_lock(Some("vendor/../skills"), skills(&["pdf"]));
    project.install(&memory).unwrap();
    assert_eq!(files_at(&project.path("skills/pdf")), installed(PDF));
    assert!(absent(&project.path("vendor")));
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
