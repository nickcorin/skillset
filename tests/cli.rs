use std::process::{Command, Output};

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
fn help_flag_prints_description_and_usage_and_exits_zero() {
    let output = skillset(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is valid UTF-8"),
        ""
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is valid UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.contains(
            &"Vendor agent skills from GitHub at pinned commits into a folder you choose."
        )
    );
    assert!(lines.contains(&"Usage: skillset"));
}

#[test]
fn no_args_prints_help_to_stderr_and_exits_two() {
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
    assert!(stderr.contains("unexpected argument 'unknown' found"));
}
