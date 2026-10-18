//! End-to-end tests running the `payments-engine` binary.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .args(args)
        .output()
        .expect("failed to run the binary")
}

/// Checks the run failed with the `errno` exit status and an error message
/// containing `message`.
fn assert_error(out: &Output, errno: libc::c_int, message: &str) {
    assert_eq!(out.status.code(), Some(errno));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: ") && stderr.contains(message),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());
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
        assert_error(
            &run(args),
            libc::EINVAL,
            "usage: payments-engine <transactions.csv>",
        );
    }
}

#[test]
fn reports_missing_file() {
    assert_error(
        &run(&["does/not/exist.csv"]),
        libc::ENOENT,
        "No such file or directory",
    );
}

#[test]
fn reports_unreadable_file() {
    // Opening a directory succeeds on Unix, but reading it fails.
    assert_error(
        &run(&[env!("CARGO_MANIFEST_DIR")]),
        libc::EISDIR,
        "Is a directory",
    );
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
    assert_error(
        &child.wait_with_output().unwrap(),
        libc::EPIPE,
        "Broken pipe",
    );
}
