use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/npx-skills")
}

fn skillset(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .current_dir(dir)
        .env("SKILLSET_TARBALL", fixtures().join("demo-skills.tar.gz"))
        .output()
        .expect("run skillset")
}

fn succeed(dir: &Path, args: &[&str]) {
    let output = skillset(dir, args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
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

#[test]
fn an_npx_lock_installs_the_files_npx_wrote() {
    let project = TempDir::new().unwrap();
    copy_lock(project.path());

    succeed(project.path(), &["install"]);

    assert_eq!(
        files(&project.path().join(".agents/skills")),
        files(&fixtures().join("installed-by-npx"))
    );
}

#[test]
fn the_flag_overrides_the_lock_which_overrides_the_default() {
    let default_dir = TempDir::new().unwrap();
    copy_lock(default_dir.path());
    succeed(default_dir.path(), &["install"]);
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
    succeed(from_lock.path(), &["install"]);
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
    succeed(from_flag.path(), &["install", "--dir", "chosen"]);
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

    let output = skillset(project.path(), &["install"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("computedHash"), "{stderr}");
    assert_eq!(tree(&skills), before);
    assert!(!skills.join("pdf-tools").exists());
}

#[test]
fn a_rerun_leaves_an_unchanged_skill_and_restores_one_that_differs() {
    let project = TempDir::new().unwrap();
    copy_lock(project.path());
    succeed(project.path(), &["install"]);

    let skills = project.path().join(".agents/skills");
    age(&skills);
    let before = tree(&skills);
    succeed(project.path(), &["install"]);
    assert_eq!(tree(&skills), before);

    fs::create_dir_all(skills.join("stray")).unwrap();
    fs::write(skills.join("stray/note.txt"), b"stray\n").unwrap();
    let skill_md = skills.join("pdf-tools/SKILL.md");
    let original = fs::read(&skill_md).unwrap();
    fs::write(&skill_md, b"changed\n").unwrap();
    age(&skills);
    let notes = tree(&skills.join("release-notes"));
    let stray = tree(&skills.join("stray"));

    succeed(project.path(), &["install"]);

    assert_eq!(fs::read(skill_md).unwrap(), original);
    assert_eq!(tree(&skills.join("release-notes")), notes);
    assert_eq!(tree(&skills.join("stray")), stray);
}
