//! End-to-end tests running the `payments-engine` binary.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .args(args)
        .output()
        .expect("failed to run the binary")
}

fn data(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn assert_output(input: &str, expected: &str) {
    let out = run(&[&data(input)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = std::fs::read_to_string(data(expected)).unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
}

#[test]
fn processes_spec_example() {
    assert_output("transactions.csv", "accounts.csv");
}

#[test]
fn processes_disputes() {
    assert_output("disputes.csv", "disputes_accounts.csv");
}

#[test]
fn rejects_wrong_argument_count() {
    for args in [&[][..], &["a.csv", "b.csv"][..]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn reports_missing_file() {
    let out = run(&["does/not/exist.csv"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does/not/exist.csv"));
}

#[test]
fn reports_unreadable_file() {
    // Opening a directory succeeds on Unix, but reading it fails.
    let out = run(&[env!("CARGO_MANIFEST_DIR")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error:"));
}

#[test]
fn reports_write_failure() {
    // Closing the read end of the pipe makes writing the balances fail.
    let mut child = Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .arg(data("transactions.csv"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the binary");
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: standard output:"));
}
