use std::process::{Command, Output};

fn skillset(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .output()
        .expect("failed to run skillset")
}

#[test]
fn version_prints_the_version_from_cargo_toml() {
    let output = skillset(&["version"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn no_command_exits_2_with_usage_on_stderr() {
    assert_usage_error(&[], "Usage: skillset <COMMAND>");
}

#[test]
fn unknown_command_exits_2_with_error_on_stderr() {
    assert_usage_error(&["unknown"], "error: unrecognized subcommand 'unknown'");
}

#[test]
fn extra_argument_after_version_exits_2_with_error_on_stderr() {
    assert_usage_error(
        &["version", "extra"],
        "error: unexpected argument 'extra' found",
    );
}

fn assert_usage_error(args: &[&str], stderr_fragment: &str) {
    let output = skillset(args);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(stderr_fragment),
        "stderr does not contain {stderr_fragment:?}:\n{stderr}"
    );
}
