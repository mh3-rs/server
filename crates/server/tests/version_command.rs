//! `mh3sp --version`: what an operator (or a bug report) reads to know which release runs. The version is the
//! workspace's, bumped by `scripts/release.sh` in the root `Cargo.toml`.

use std::process::{Command, Output};

fn mh3sp(args: &[&str]) -> Output {
    // An empty folder: no `mh3sp.toml` to find, so nothing here may depend on the configuration.
    let dir = std::env::temp_dir().join(format!("mh3sp-version-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Command::new(env!("CARGO_BIN_EXE_mh3sp"))
        .args(args)
        .current_dir(&dir)
        .output()
        .unwrap()
}

/// The root manifest's own `version = "X.Y.Z"` line (dependencies' versions are inside braces, never at a line's
/// start).
fn workspace_version() -> String {
    include_str!("../../../Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("the workspace version in the root Cargo.toml")
        .to_owned()
}

#[test]
fn version_prints_the_workspace_version() {
    let out = mh3sp(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("mh3sp {}\n", workspace_version())
    );
}

/// The failure path stays: a first argument the command doesn't know exits 1 with the usage, which names `--version`.
#[test]
fn an_unknown_argument_exits_1_with_the_usage() {
    let out = mh3sp(&["--versoin"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("usage: mh3sp ["), "{err}");
    assert!(err.contains("--version"), "{err}");
}
