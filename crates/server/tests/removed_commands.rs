//! The commands that left the binary: each now exits 1 with the usage, which no longer names it. (An operator's script
//! that still calls one fails loudly, not silently.)

use std::process::{Command, Output};

fn mh3sp(args: &[&str]) -> Output {
    // An empty folder: no `mh3sp.toml` to find, so nothing here may depend on the configuration.
    let dir = std::env::temp_dir().join(format!("mh3sp-removed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Command::new(env!("CARGO_BIN_EXE_mh3sp"))
        .args(args)
        .current_dir(&dir)
        .output()
        .unwrap()
}

/// The usage the binary prints for what it doesn't know. It still names the commands that exist: a usage that said
/// nothing would pass the checks that follow it too.
fn usage() -> String {
    let out = mh3sp(&["--nothing"]);
    let usage = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        usage.contains("usage: mh3sp [") && usage.contains("announce"),
        "{usage}"
    );
    usage
}

/// Each of these exits 1 with the usage, as a command the binary never had.
fn fall_to_the_usage(commands: &[&[&str]]) {
    for args in commands {
        let out = mh3sp(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("usage: mh3sp ["), "{args:?}: {err}");
    }
}

/// The quest bots were removed (tag `archive/quest-bots`): `mh3sp bot add` and `bot remove`.
#[test]
fn the_bot_commands_are_gone() {
    assert!(!usage().contains("bot"));
    fall_to_the_usage(&[&["bot", "add", "ABC123"], &["bot", "remove", "0BOT01"]]);
}

/// The API's users and tokens were removed: `mh3sp user add`, `user password`, `token add` and `token revoke`.
#[test]
fn the_user_and_token_commands_are_gone() {
    let usage = usage();
    assert!(
        !usage.contains("user") && !usage.contains("token"),
        "{usage}"
    );
    fall_to_the_usage(&[
        &["user", "add", "admin@example.org", "admin"],
        &["user", "password", "admin@example.org"],
        &["token", "add", "admin@example.org"],
        &["token", "revoke", "mh3sp_0123abcd"],
    ]);
}
