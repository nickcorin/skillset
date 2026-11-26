use std::process::Command;

#[test]
fn version_prints_the_version_from_cargo_toml() {
    let output = Command::new(env!("CARGO_BIN_EXE_skillset"))
        .arg("version")
        .output()
        .expect("failed to run skillset");

    assert!(
        output.status.success(),
        "skillset version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("version output is utf-8");
    assert_eq!(stdout, format!("{}\n", env!("CARGO_PKG_VERSION")));
    assert!(output.stderr.is_empty());
}
