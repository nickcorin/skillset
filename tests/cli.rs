use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const HELP_OUTPUT: &str = "\
Vendor agent skills from GitHub at pinned commits into a folder you choose.

Usage: skillset <COMMAND>

Commands:
  install  
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
";

fn skillset(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .output()
        .expect("failed to execute skillset binary")
}

fn workspace() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("skillset-cli-{}-{n}", std::process::id()));
    fs::create_dir_all(&path).expect("workspace");
    path
}

fn skillset_in(dir: &std::path::Path, args: &[&str]) -> Output {
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
fn unexpected_positional_argument_prints_error_to_stderr_and_exits_two() {
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
fn install_without_lock_exits_nonzero_and_does_not_create_the_default_dir() {
    let dir = workspace();
    let output = skillset_in(&dir, &["install"]);
    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");

    assert_ne!(output.status.code(), Some(0));
    assert!(stderr.contains("skills-lock.json"), "{stderr}");
    assert!(!dir.join(".agents/skills").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn install_without_lock_does_not_create_the_requested_dir() {
    let dir = workspace();
    let dest = dir.join("elsewhere");
    let output = skillset_in(&dir, &["install", "--dir", dest.to_str().expect("utf-8")]);

    assert_ne!(output.status.code(), Some(0));
    assert!(!dest.exists());
    assert!(!dir.join(".agents/skills").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn version_one_lock_exits_nonzero_and_does_not_create_the_install_dir() {
    let dir = workspace();
    fs::write(
        dir.join("skills-lock.json"),
        r#"{"version":1,"skills":{"My Skill":{"source":"owner/repo"}}}"#,
    )
    .expect("lock");
    let output = skillset_in(&dir, &["install"]);
    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");

    assert_ne!(output.status.code(), Some(0));
    assert!(stderr.contains("unsupported lock file"), "{stderr}");
    assert!(stderr.contains("version 1"), "{stderr}");
    assert!(!stderr.contains("commit"), "{stderr}");
    assert!(!dir.join(".agents/skills").exists());
    let _ = fs::remove_dir_all(&dir);
}
