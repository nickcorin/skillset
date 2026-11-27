use std::process::{Command, Output};

fn run_skillset(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .output()
        .expect("failed to execute skillset binary")
}

#[test]
fn version_prints_crate_version_and_exits_zero() {
    let output = run_skillset(&["version"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn no_command_prints_usage_to_stderr_and_exits_two() {
    let output = run_skillset(&[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: skillset <command>"));
    assert!(stderr.contains("version"));
}

#[test]
fn unknown_command_prints_error_to_stderr_and_exits_two() {
    let output = run_skillset(&["unknown"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command 'unknown'"));
    assert!(stderr.contains("Usage: skillset <command>"));
}

#[test]
fn extra_arguments_to_version_print_error_to_stderr_and_exit_two() {
    let output = run_skillset(&["version", "extra"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument 'extra'"));
    assert!(stderr.contains("Usage: skillset <command>"));
}

#[test]
fn multiple_extra_arguments_to_version_exit_two() {
    let output = run_skillset(&["version", "one", "two"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument 'one'"));
}
