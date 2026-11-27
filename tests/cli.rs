use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const HELP_OUTPUT: &str = "\
Vendor agent skills from GitHub at pinned commits into a folder you choose.

Usage: skillset <COMMAND>

Commands:
  install  Restore every skill in skills-lock.json at the commit the lock records
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
";

const INSTALL_HELP_OUTPUT: &str = "\
Restore every skill in skills-lock.json at the commit the lock records

Usage: skillset install [OPTIONS]

Options:
      --lock <FILE>  Lock file to restore from [default: skills-lock.json]
      --dir <DIR>    Install into this folder instead of the lock's skillset.installDir or .agents/skills
  -h, --help         Print help
";

fn skillset(args: &[&str]) -> Output {
    skillset_in(Path::new("."), args)
}

fn skillset_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to execute skillset binary")
}

#[test]
fn version_flags_print_version_and_exit_zero() {
    let expected = format!("skillset {}\n", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        let output = skillset(&[flag]);

        assert_eq!(output.status.code(), Some(0), "flag: {flag}");
        assert_eq!(
            String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
            expected,
            "flag: {flag}"
        );
        assert_eq!(
            String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
            "",
            "flag: {flag}"
        );
    }
}

#[test]
fn help_flags_print_help_and_exit_zero() {
    for flag in ["--help", "-h"] {
        let output = skillset(&[flag]);

        assert_eq!(output.status.code(), Some(0), "flag: {flag}");
        assert_eq!(
            String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
            HELP_OUTPUT,
            "flag: {flag}"
        );
        assert_eq!(
            String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
            "",
            "flag: {flag}"
        );
    }
}

#[test]
fn no_args_prints_help_to_stderr_and_exits_two() {
    let output = skillset(&[]);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        HELP_OUTPUT
    );
}

#[test]
fn unknown_subcommand_prints_error_to_stderr_and_exits_two() {
    let output = skillset(&["unknown"]);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        "\
error: unrecognized subcommand 'unknown'

Usage: skillset <COMMAND>

For more information, try '--help'.
"
    );
}

#[test]
fn unknown_flag_prints_error_to_stderr_and_exits_two() {
    let output = skillset(&["--bogus"]);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        "\
error: unexpected argument '--bogus' found

Usage: skillset <COMMAND>

For more information, try '--help'.
"
    );
}

#[test]
fn install_help_prints_options_and_exits_zero() {
    let output = skillset(&["install", "--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        INSTALL_HELP_OUTPUT
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
}

#[test]
fn install_with_malformed_lock_prints_error_and_exits_one() {
    let project = TempDir::new().expect("temp dir");
    fs::write(
        project.path().join("skills-lock.json"),
        r#"{"version": 2, "skills": {"pdf": {"sourceType": "github"}}}"#,
    )
    .expect("write lock");

    let output = skillset_in(project.path(), &["install"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");
    assert!(
        stderr.starts_with("error: skills-lock.json:") && stderr.contains("source"),
        "{stderr}"
    );
    assert!(!project.path().join(".agents").exists());
}

#[test]
fn install_without_lock_prints_error_and_exits_one() {
    let project = TempDir::new().expect("temp dir");

    let output = skillset_in(project.path(), &["install", "--lock", "missing.json"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");
    assert!(
        stderr.starts_with("error: cannot read missing.json: "),
        "{stderr}"
    );
}

#[test]
fn install_with_empty_lock_prints_nothing_and_exits_zero() {
    let project = TempDir::new().expect("temp dir");
    fs::write(
        project.path().join("skills-lock.json"),
        r#"{"version": 2, "skills": {}}"#,
    )
    .expect("write lock");

    let output = skillset_in(project.path(), &["install", "--dir", "vendor"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        ""
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
    assert!(!project.path().join("vendor").exists());
}
