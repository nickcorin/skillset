use std::process::{Command, Output};

fn skillset(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .output()
        .expect("failed to execute skillset binary")
}

#[test]
fn version_prints_crate_version_and_exits_zero() {
    let output = skillset(&["version"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
}

#[test]
fn no_command_prints_usage_to_stderr_and_exits_two() {
    let output = skillset(&[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");
    assert!(stderr.contains("Usage: skillset"));
}

#[test]
fn unknown_command_prints_error_to_stderr_and_exits_two() {
    let output = skillset(&["unknown"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");
    assert!(stderr.contains("unrecognized subcommand 'unknown'"));
}

#[test]
fn extra_argument_to_version_prints_error_to_stderr_and_exits_two() {
    let output = skillset(&["version", "extra"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).expect("stderr is valid UTF-8");
    assert!(stderr.contains("unexpected argument 'extra'"));
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    let output = skillset(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8(output.stdout).expect("stdout is valid UTF-8");
    assert!(stdout.contains("Usage: skillset"));
    assert!(stdout.contains("version"));
}

#[test]
fn version_flag_prints_version_and_exits_zero() {
    let output = skillset(&["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8(output.stdout).expect("stdout is valid UTF-8");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}
