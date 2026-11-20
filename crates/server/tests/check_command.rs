//! `mh3sp check` as an operator's cron job runs it: its exit code is what a monitor reads.

use std::process::{Command, Output};

fn check(domain: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mh3sp"))
        .args(["check", domain])
        .output()
        .unwrap()
}

/// A domain that can't resolve (`.invalid`, RFC 6761): every check fails, and the run exits 1. A command that always
/// exited 0 would pass every other test.
#[test]
fn a_failed_check_exits_1() {
    let out = check("mh3.example.invalid");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("FAIL  DNS mh3.example.invalid"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
}

/// A name that isn't a domain (here with a line break, which would reach the HTTP request) is refused before any
/// lookup, with the configuration's own rule.
#[test]
fn a_domain_that_is_not_one_is_refused_before_anything_is_sent() {
    let out = check("mh3.example.org\r\nX: y");
    assert!(out.stdout.is_empty(), "no check ran");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a domain name"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
