use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde_json::{Value, json};
use skillset::content_hash::ContentHash;
use skillset::fetch::{FetchError, GitHubTarballFetcher, SourceFetcher};
use skillset::install::{InstallCommand, InstallError, InstallReport, InstallWarning, SkillChange};
use skillset::lock::{CommitSha, GitHubRepo, Lockfile};
use skillset::snapshot::{RepoSnapshot, SkillFile};
use tempfile::TempDir;

const REPO: &str = "acme/agent-skills";
const FIRST: &str = "1111111111111111111111111111111111111111";
const SECOND: &str = "2222222222222222222222222222222222222222";
const MIT_LICENSE: &[u8] = b"MIT License\n\nCopyright (c) 2026 Acme\n";

/// A repository at one commit, as the fake GitHub serves it.
#[derive(Clone)]
struct SourceCommit {
    repo: &'static str,
    sha: &'static str,
    files: BTreeMap<String, SkillFile>,
}

impl SourceCommit {
    /// A repository with a license, a README and two skills: `pdf` (nested
    /// folders, an executable, CRLF text and binary data) and `notes`.
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

    fn snapshot(&self) -> RepoSnapshot {
        let mut snapshot = RepoSnapshot::new();
        for (path, file) in &self.files {
            snapshot.insert_file(path, file.clone()).unwrap();
        }
        snapshot
    }

    /// The files of `skills/<skill>/`, keyed relative to that folder, as they
    /// should appear once installed on this platform.
    fn skill_files(&self, skill: &str) -> BTreeMap<String, SkillFile> {
        let prefix = format!("skills/{skill}/");
        self.files
            .iter()
            .filter_map(|(path, file)| {
                let relative = path.strip_prefix(&prefix)?;
                let installed = SkillFile {
                    contents: file.contents.clone(),
                    executable: file.executable && cfg!(unix),
                };
                Some((relative.to_owned(), installed))
            })
            .collect()
    }

    /// The `skills-lock.json` entry `npx skills add` writes for `skills/<skill>/`.
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

/// A `skills-lock.json` being built for a test.
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

/// Serves source snapshots from memory and counts how often it is asked.
struct FakeGitHub {
    snapshots: BTreeMap<(String, String), RepoSnapshot>,
    fetches: Cell<usize>,
}

impl FakeGitHub {
    fn new() -> Self {
        Self {
            snapshots: BTreeMap::new(),
            fetches: Cell::new(0),
        }
    }

    fn serving(self, source: &SourceCommit) -> Self {
        self.serving_snapshot(source.repo, source.sha, source.snapshot())
    }

    fn serving_snapshot(mut self, repo: &str, commit: &str, snapshot: RepoSnapshot) -> Self {
        self.snapshots
            .insert((repo.to_owned(), commit.to_owned()), snapshot);
        self
    }
}

impl SourceFetcher for &FakeGitHub {
    fn fetch(&self, repo: &GitHubRepo, commit: &CommitSha) -> Result<RepoSnapshot, FetchError> {
        self.fetches.set(self.fetches.get() + 1);
        self.snapshots
            .get(&(repo.to_string(), commit.to_string()))
            .cloned()
            .ok_or_else(|| FetchError::Status {
                url: GitHubTarballFetcher::tarball_url(repo, commit),
                status: 404,
            })
    }
}

/// A project folder holding a `skills-lock.json`.
struct Project {
    root: TempDir,
}

/// One path in a project, as far as tests compare them.
#[derive(Debug, PartialEq, Eq)]
enum Node {
    Folder,
    File {
        contents: Vec<u8>,
        modified: SystemTime,
    },
}

impl Project {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn lock_path(&self) -> PathBuf {
        self.path("skills-lock.json")
    }

    fn write_lock(&self, lock: &Lock) {
        fs::write(self.lock_path(), format!("{:#}\n", lock.json)).unwrap();
    }

    fn install(&self, github: &FakeGitHub) -> Result<InstallReport, InstallError> {
        InstallCommand::new(self.lock_path(), None, github).install()
    }

    fn install_into(
        &self,
        github: &FakeGitHub,
        dir: PathBuf,
    ) -> Result<InstallReport, InstallError> {
        InstallCommand::new(self.lock_path(), Some(dir), github).install()
    }

    /// The regular files under `relative`, keyed by `/`-separated relative path.
    fn files(&self, relative: &str) -> BTreeMap<String, SkillFile> {
        Self::files_at(&self.path(relative))
    }

    fn files_at(folder: &Path) -> BTreeMap<String, SkillFile> {
        let mut files = BTreeMap::new();
        let mut pending = vec![(folder.to_path_buf(), String::new())];
        while let Some((folder, prefix)) = pending.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let entry = entry.unwrap();
                let name = format!("{prefix}{}", entry.file_name().into_string().unwrap());
                if entry.file_type().unwrap().is_dir() {
                    pending.push((entry.path(), format!("{name}/")));
                    continue;
                }
                #[cfg(unix)]
                let executable = entry.metadata().unwrap().permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let executable = false;
                let contents = fs::read(entry.path()).unwrap();
                files.insert(
                    name,
                    SkillFile {
                        contents,
                        executable,
                    },
                );
            }
        }
        files
    }

    /// The names directly inside `relative`, sorted.
    fn entries(&self, relative: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path(relative))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Every path in the project with its contents and modification time.
    fn state(&self) -> BTreeMap<PathBuf, Node> {
        let mut state = BTreeMap::new();
        let mut pending = vec![self.root.path().to_path_buf()];
        while let Some(folder) = pending.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(self.root.path()).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    state.insert(relative, Node::Folder);
                    pending.push(path);
                } else {
                    let modified = entry.metadata().unwrap().modified().unwrap();
                    let contents = fs::read(&path).unwrap();
                    state.insert(relative, Node::File { contents, modified });
                }
            }
        }
        state
    }

    /// Sets the modification time of every file to a fixed time in the past, so a
    /// later rewrite shows up even within the file system's time resolution.
    fn age_files(&self) {
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for (relative, node) in self.state() {
            if let Node::File { .. } = node {
                let file = fs::File::options()
                    .write(true)
                    .open(self.root.path().join(relative))
                    .unwrap();
                file.set_modified(past).unwrap();
            }
        }
    }
}

trait ReportSummary {
    fn changes(&self) -> Vec<(&str, SkillChange)>;
}

impl ReportSummary for InstallReport {
    fn changes(&self) -> Vec<(&str, SkillChange)> {
        self.skills
            .iter()
            .map(|outcome| (outcome.folder_name.as_str(), outcome.change))
            .collect()
    }
}

#[test]
fn installs_files_byte_identical_into_default_folder() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));

    let report = project.install(&github).unwrap();

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
        [".licenses", ".skillset-manifest.json", "notes", "pdf"]
    );
    assert_eq!(report.install_dir, project.path(".agents/skills"));
    assert_eq!(
        report.changes(),
        [("notes", SkillChange::Added), ("pdf", SkillChange::Added)]
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn install_dir_in_lock_replaces_default_folder() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("vendor/skills")
            .skill("pdf", &source),
    );

    let report = project.install(&github).unwrap();

    assert_eq!(report.install_dir, project.path("vendor/skills"));
    assert_eq!(
        project.files("vendor/skills/pdf"),
        source.skill_files("pdf")
    );
    assert!(!project.path(".agents").exists());
}

#[test]
fn dir_override_replaces_lock_and_default_folders() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(
        &Lock::new()
            .install_dir("vendor/skills")
            .skill("pdf", &source),
    );

    let report = project
        .install_into(&github, project.path("chosen"))
        .unwrap();

    assert_eq!(report.install_dir, project.path("chosen"));
    assert_eq!(project.files("chosen/pdf"), source.skill_files("pdf"));
    assert!(!project.path("vendor").exists());
    assert!(!project.path(".agents").exists());
}

#[test]
fn installs_source_license_beside_each_skill() {
    let source = SourceCommit::demo(FIRST).with_file("COPYING", b"GPL text\n");
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));

    project.install(&github).unwrap();

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
fn warns_when_source_has_no_license() {
    let source = SourceCommit::demo(FIRST).without("LICENSE");
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));

    let report = project.install(&github).unwrap();

    assert!(
        matches!(report.warnings.as_slice(), [InstallWarning::NoLicenseFile { skill, .. }] if skill == "pdf"),
        "{:?}",
        report.warnings
    );
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
    assert!(!project.path(".agents/skills/.licenses").exists());
}

#[test]
fn hash_mismatch_fails_and_leaves_install_folder_untouched() {
    let first = SourceCommit::demo(FIRST);
    let second = SourceCommit::demo(SECOND)
        .with_file("skills/pdf/SKILL.md", b"# PDF, second commit\n")
        .with_file("skills/notes/SKILL.md", b"# Notes, second commit\n");
    let github = FakeGitHub::new().serving(&first).serving(&second);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &first).skill("notes", &first));
    project.install(&github).unwrap();

    let mut tampered = second.lock_entry("pdf");
    tampered["computedHash"] = first.lock_entry("pdf")["computedHash"].clone();
    project.write_lock(&Lock::new().skill("notes", &second).entry("pdf", tampered));
    let before = project.state();

    let err = project.install(&github).unwrap_err();

    assert!(
        matches!(&err, InstallError::HashMismatch { entry, .. } if entry.name == "pdf"),
        "{err}"
    );
    assert_eq!(project.state(), before);
}

#[test]
fn hash_mismatch_on_first_install_creates_nothing() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    let mut tampered = source.lock_entry("pdf");
    tampered["computedHash"] = json!("0".repeat(64));
    project.write_lock(&Lock::new().skill("notes", &source).entry("pdf", tampered));
    let before = project.state();

    let err = project.install(&github).unwrap_err();

    assert!(matches!(err, InstallError::HashMismatch { .. }), "{err}");
    assert_eq!(project.state(), before);
    assert!(!project.path(".agents").exists());
}

#[test]
fn second_run_with_unchanged_lock_writes_nothing() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));
    project.install(&github).unwrap();
    project.age_files();
    let before = project.state();

    let report = project.install(&github).unwrap();

    assert_eq!(project.state(), before);
    assert_eq!(
        report.changes(),
        [
            ("notes", SkillChange::Unchanged),
            ("pdf", SkillChange::Unchanged)
        ]
    );
}

#[test]
fn removes_skill_dropped_from_lock_and_keeps_unmanaged_folders() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));
    project.install(&github).unwrap();
    fs::create_dir(project.path(".agents/skills/my-own")).unwrap();
    fs::write(project.path(".agents/skills/my-own/SKILL.md"), "# mine\n").unwrap();

    project.write_lock(&Lock::new().skill("pdf", &source));
    let report = project.install(&github).unwrap();

    assert_eq!(
        project.entries(".agents/skills"),
        [".licenses", ".skillset-manifest.json", "my-own", "pdf"]
    );
    assert_eq!(project.entries(".agents/skills/.licenses"), ["pdf"]);
    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
    assert_eq!(
        project.files(".agents/skills/my-own"),
        BTreeMap::from([("SKILL.md".to_owned(), SkillFile::new(*b"# mine\n"))])
    );
    assert_eq!(
        report.changes(),
        [
            ("notes", SkillChange::Removed),
            ("pdf", SkillChange::Unchanged)
        ]
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(project.path(".agents/skills/.skillset-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest, json!({ "version": 1, "skills": ["pdf"] }));
}

#[test]
fn removing_every_skill_leaves_only_unmanaged_folders() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&github).unwrap();
    fs::create_dir(project.path(".agents/skills/my-own")).unwrap();

    project.write_lock(&Lock::new());
    let report = project.install(&github).unwrap();

    assert_eq!(project.entries(".agents/skills"), ["my-own"]);
    assert_eq!(report.changes(), [("pdf", SkillChange::Removed)]);
}

#[test]
fn new_commit_replaces_skill_folder_exactly() {
    let first = SourceCommit::demo(FIRST);
    let second = SourceCommit::demo(SECOND)
        .without("skills/pdf/docs/crlf.md")
        .with_file("skills/pdf/docs/added.md", b"added\n")
        .with_file("LICENSE", b"Apache License\n");
    let github = FakeGitHub::new().serving(&first).serving(&second);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &first).skill("notes", &first));
    project.install(&github).unwrap();

    project.write_lock(&Lock::new().skill("pdf", &second).skill("notes", &first));
    let report = project.install(&github).unwrap();

    assert_eq!(
        project.files(".agents/skills/pdf"),
        second.skill_files("pdf")
    );
    assert_eq!(
        project.files(".agents/skills/.licenses/pdf"),
        BTreeMap::from([("LICENSE".to_owned(), SkillFile::new(*b"Apache License\n"))])
    );
    assert_eq!(
        report.changes(),
        [
            ("notes", SkillChange::Unchanged),
            ("pdf", SkillChange::Updated)
        ]
    );
    let pdf = report
        .skills
        .iter()
        .find(|outcome| outcome.folder_name.as_str() == "pdf")
        .unwrap();
    let (repo, commit) = pdf.source.as_ref().unwrap();
    assert_eq!(
        (repo.to_string(), commit.to_string()),
        (REPO.to_owned(), SECOND.to_owned())
    );
}

#[test]
fn restores_locally_modified_skill() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    project.install(&github).unwrap();
    fs::write(project.path(".agents/skills/pdf/SKILL.md"), "edited\n").unwrap();
    fs::write(project.path(".agents/skills/pdf/stray.txt"), "stray\n").unwrap();
    fs::create_dir(project.path(".agents/skills/pdf/empty")).unwrap();

    let report = project.install(&github).unwrap();

    assert_eq!(
        project.files(".agents/skills/pdf"),
        source.skill_files("pdf")
    );
    assert!(!project.path(".agents/skills/pdf/empty").exists());
    assert_eq!(report.changes(), [("pdf", SkillChange::Updated)]);
}

#[test]
fn installs_lock_written_by_npx_skills_like_npx_does() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills");
    let tarball = fs::read(fixtures.join("demo-skills.tar.gz")).unwrap();
    let snapshot = RepoSnapshot::from_tar_gz(tarball.as_slice()).unwrap();
    let github = FakeGitHub::new().serving_snapshot(
        "skillset-fixture-owner/demo-skills",
        "b45dbefda7e1098d90f140b803059d6ecbc367dc",
        snapshot,
    );
    let project = Project::new();
    fs::copy(fixtures.join("skills-lock.json"), project.lock_path()).unwrap();

    let report = project.install(&github).unwrap();

    let installed: BTreeMap<String, SkillFile> = project
        .files(".agents/skills")
        .into_iter()
        .filter(|(path, _)| !path.starts_with('.'))
        .collect();
    assert_eq!(
        installed,
        Project::files_at(&fixtures.join("installed-by-npx"))
    );
    assert_eq!(
        report.changes(),
        [
            ("pdf-tools", SkillChange::Added),
            ("release-notes", SkillChange::Added)
        ]
    );

    let mut archive = tar::Archive::new(GzDecoder::new(tarball.as_slice()));
    let mut license = Vec::new();
    archive
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path().unwrap().ends_with("LICENSE"))
        .unwrap()
        .read_to_end(&mut license)
        .unwrap();
    for skill in ["pdf-tools", "release-notes"] {
        assert_eq!(
            project.files(&format!(".agents/skills/.licenses/{skill}")),
            BTreeMap::from([("LICENSE".to_owned(), SkillFile::new(license.clone()))])
        );
    }
}

#[test]
fn reads_lock_written_by_npx_skills_for_github_repositories() {
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
fn fetches_each_repository_commit_once() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source).skill("notes", &source));

    project.install(&github).unwrap();

    assert_eq!(github.fetches.get(), 1);
}

#[test]
fn unsupported_lock_entry_fails_before_fetching_or_writing() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    let local =
        json!({ "source": "./my-skills", "sourceType": "local", "computedHash": "0".repeat(64) });
    project.write_lock(&Lock::new().skill("pdf", &source).entry("mine", local));
    let before = project.state();

    let err = project.install(&github).unwrap_err();

    assert!(matches!(err, InstallError::Lock(_)), "{err}");
    assert_eq!(github.fetches.get(), 0);
    assert_eq!(project.state(), before);
}

#[test]
fn fetch_failure_leaves_install_folder_untouched() {
    let first = SourceCommit::demo(FIRST);
    let unpublished = SourceCommit::demo(SECOND);
    let github = FakeGitHub::new().serving(&first);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &first));
    project.install(&github).unwrap();
    project.write_lock(&Lock::new().skill("pdf", &unpublished));
    let before = project.state();

    let err = project.install(&github).unwrap_err();

    assert!(matches!(err, InstallError::Fetch { .. }), "{err}");
    assert_eq!(project.state(), before);
}

#[test]
fn skill_missing_from_source_fails() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    let mut entry = source.lock_entry("pdf");
    entry["skillPath"] = json!("skills/absent/SKILL.md");
    project.write_lock(&Lock::new().entry("absent", entry));

    let err = project.install(&github).unwrap_err();

    assert!(matches!(err, InstallError::MissingSkill { .. }), "{err}");
    assert!(!project.path(".agents").exists());
}

#[test]
fn unreadable_manifest_fails_without_changes() {
    let source = SourceCommit::demo(FIRST);
    let github = FakeGitHub::new().serving(&source);
    let project = Project::new();
    project.write_lock(&Lock::new().skill("pdf", &source));
    fs::create_dir_all(project.path(".agents/skills")).unwrap();
    fs::write(
        project.path(".agents/skills/.skillset-manifest.json"),
        "{\"version\": 1, \"skills\": [\"../x\"]}",
    )
    .unwrap();
    let before = project.state();

    let err = project.install(&github).unwrap_err();

    assert!(matches!(err, InstallError::Manifest { .. }), "{err}");
    assert_eq!(project.state(), before);
}
