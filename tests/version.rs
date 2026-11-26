use std::process::{Command, Output};

const USAGE: &str = "\
usage: skillset <command>

commands:
  version    print the version this binary was built with
";

fn skillset(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skillset"))
        .args(args)
        .output()
        .expect("failed to run skillset")
}

#[test]
fn version_prints_the_version_from_cargo_toml() {
    let output = skillset(&["version"]);

    assert!(
        output.status.success(),
        "skillset version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("version output is utf-8");
    assert_eq!(stdout, format!("{}\n", env!("CARGO_PKG_VERSION")));
    assert!(output.stderr.is_empty());
}

#[test]
fn no_command_exits_2_with_usage_on_stderr() {
    assert_exit_2(&[], USAGE);
}

#[test]
fn unknown_command_exits_2_with_error_on_stderr() {
    assert_exit_2(
        &["unknown"],
        &format!("unrecognized command `unknown`\n{USAGE}"),
    );
}

#[test]
fn extra_args_after_version_exit_2_with_usage_on_stderr() {
    assert_exit_2(&["version", "extra"], "usage: skillset version\n");
}

fn assert_exit_2(args: &[&str], stderr: &str) {
    let output = skillset(args);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let actual = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert_eq!(actual, stderr);
}
