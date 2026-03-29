// Test that the binary correctly prints its version via `--version`
#[test]
fn test_version_flag() {
    // `CARGO_BIN_EXE_github-commit-info` is set by Cargo when running integration tests
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_github-commit-info"))
        .arg("--version")
        .output()
        .expect("failed to execute binary with --version");
    assert!(
        output.status.success(),
        "binary exited with non-zero status"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The version string should contain the package version from Cargo.toml
    let expected = env!("CARGO_PKG_VERSION");
    assert!(
        stdout.contains(expected),
        "output did not contain version {}: {}",
        expected,
        stdout
    );
}
