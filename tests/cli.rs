use std::process::{Command, Output};

const HELP_OUTPUT: &str = "\
Vendor agent skills from GitHub at pinned commits into a folder you choose.

Usage: skillset

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

#[test]
fn version_long_flag_prints_version_and_exits_zero() {
    let output = skillset(&["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        format!("skillset {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
}

#[test]
fn version_short_flag_prints_version_and_exits_zero() {
    let output = skillset(&["-V"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        format!("skillset {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
}

#[test]
fn help_long_flag_prints_help_and_exits_zero() {
    let output = skillset(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        HELP_OUTPUT
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
}

#[test]
fn help_short_flag_prints_help_and_exits_zero() {
    let output = skillset(&["-h"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is valid UTF-8"),
        HELP_OUTPUT
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );
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
error: unexpected argument 'unknown' found

Usage: skillset

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

Usage: skillset

For more information, try '--help'.
"
    );
}
