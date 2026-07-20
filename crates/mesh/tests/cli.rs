//! End-to-end tests that drive the built `mesh` binary.
//!
//! No test-harness crates: Cargo exposes the binary path as `CARGO_BIN_EXE_mesh`
//! to integration tests, so std is enough. Input is piped on stdin (making the
//! shell non-interactive, so no prompt is written), and we assert on stdout,
//! stderr, and the exit code.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn run_with_input(input: &str) -> Output {
    run_with_bytes(input.as_bytes())
}

/// A fresh, empty temp directory unique to this test process and `tag`.
fn fresh_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("mesh_test_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run mesh with `HOME` set to `home` (for tilde tests).
fn run_with_home(input: &str, home: &std::path::Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for mesh")
}

fn run_with_bytes(input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write stdin");
    child.wait_with_output().expect("wait for mesh")
}

#[test]
fn runs_an_external_command() {
    let out = run_with_input("echo hello\n");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
}

#[test]
fn arguments_are_passed_through() {
    let out = run_with_input("echo one two   three\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one two three\n");
}

#[test]
fn blank_and_whitespace_lines_are_ignored() {
    let out = run_with_input("\n   \t\necho ok\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn missing_command_reports_127() {
    let out = run_with_input("this_command_does_not_exist_42\n");
    assert_eq!(out.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&out.stderr).contains("command not found"));
}

#[test]
fn exit_status_propagates() {
    let out = run_with_input("exit 3\n");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn exit_status_is_masked_to_eight_bits() {
    assert_eq!(run_with_input("exit 256\n").status.code(), Some(0));
    assert_eq!(run_with_input("exit -1\n").status.code(), Some(255));
    assert_eq!(run_with_input("exit 257\n").status.code(), Some(1));
}

#[test]
fn exit_rejects_surplus_operands_without_exiting() {
    // A typo like `exit 3 junk` should not terminate the shell; the following
    // command still runs, so the shell exits with echo's status (0), not 3.
    let out = run_with_input("exit 3 junk\necho still here\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "still here\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("too many arguments"));
}

#[test]
fn pwd_prints_the_working_directory() {
    let out = run_with_input("cd /\npwd\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/\n");
}

#[test]
fn pwd_rejects_operands() {
    let out = run_with_input("pwd extra\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("too many arguments"));
}

#[test]
fn puts_joins_arguments_with_spaces() {
    let out = run_with_input("puts hello   world\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world\n");
}

#[test]
fn puts_with_no_arguments_prints_a_blank_line() {
    let out = run_with_input("puts\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\n");
}

#[test]
fn cd_updates_pwd_and_oldpwd_for_children() {
    let out = run_with_input("cd /\nprintenv PWD\ncd /usr\nprintenv OLDPWD\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/\n/\n");
}

#[test]
fn cd_dash_returns_to_previous_and_prints_it() {
    // cd /usr, cd /, then `cd -` goes back to /usr and echoes it.
    let out = run_with_input("cd /usr\ncd /\ncd -\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/usr\n");
}

#[test]
fn cd_rejects_surplus_operands() {
    let out = run_with_input("cd / extra\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("too many arguments"));
}

#[test]
fn glob_expands_and_sorts_matches() {
    let dir = fresh_dir("glob_match");
    std::fs::write(dir.join("b.ext"), "").unwrap();
    std::fs::write(dir.join("a.ext"), "").unwrap();
    std::fs::write(dir.join("c.other"), "").unwrap();
    let out = run_with_input(&format!("cd {}\nputs *.ext\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.ext b.ext\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glob_with_no_matches_contributes_nothing() {
    let dir = fresh_dir("glob_empty");
    // The middle word globs to nothing, so `puts` sees only `x` and `y`.
    let out = run_with_input(&format!("cd {}\nputs x *.nomatch y\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x y\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_glob_word_passes_through_even_if_absent() {
    let dir = fresh_dir("glob_literal");
    let out = run_with_input(&format!("cd {}\nputs missing.txt\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "missing.txt\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tilde_expands_to_home() {
    let home = fresh_dir("tilde_home");
    let out = run_with_home("puts ~\n", &home);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", home.display())
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn cd_tilde_goes_home() {
    let home = fresh_dir("tilde_cd");
    let out = run_with_home("cd ~\npwd\n", &home);
    // pwd reports the canonical getcwd, so canonicalize the expected path too —
    // otherwise this fails where the temp dir sits under a symlink (macOS
    // /var -> /private/var).
    let expected = home.canonicalize().expect("canonicalize home");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", expected.display())
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn glob_star_excludes_dotfiles() {
    let dir = fresh_dir("glob_dot");
    std::fs::write(dir.join("visible.txt"), "").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    let out = run_with_input(&format!("cd {}\nputs *\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "visible.txt\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_command_that_globs_away_reports_success() {
    let dir = fresh_dir("glob_away");
    // `false` sets status 1; a line that globs to nothing is an empty-list
    // result and must reset to 0 (not preserve the previous status).
    let out = run_with_input(&format!(
        "cd {}\nfalse\n*.definitely_missing\n",
        dir.display()
    ));
    assert_eq!(out.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_blank_line_preserves_the_previous_status() {
    // A truly blank line is not a command, so it leaves the status untouched.
    let out = run_with_input("false\n\n");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn tilde_keeps_home_metacharacters_literal() {
    // A $HOME containing glob metacharacters must not be treated as a pattern.
    let base = fresh_dir("tilde_meta");
    let home = base.join("home[1]");
    std::fs::create_dir_all(&home).unwrap();
    let out = run_with_home("puts ~\n", &home);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", home.display())
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(target_os = "linux")]
#[test]
fn stdout_write_error_does_not_crash_the_shell() {
    // Writing to /dev/full always fails with ENOSPC. `puts` must report the
    // error and the REPL must keep going (not panic with exit 101), so the
    // following `exit 7` still runs.
    use std::fs::OpenOptions;
    let dev_full = OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .stdin(Stdio::piped())
        .stdout(dev_full)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"puts hi\nexit 7\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(out.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&out.stderr).contains("puts"));
}

#[test]
fn last_status_becomes_the_exit_code() {
    // `false` exits 1, then EOF; the shell should exit 1.
    let out = run_with_input("false\n");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn invalid_utf8_line_is_rejected_loudly() {
    // A malformed line is reported and skipped, not lossily executed; the shell
    // recovers and runs the next line.
    let out = run_with_bytes(b"\xff\xfe\necho ok\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid UTF-8"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn child_reads_remaining_stdin() {
    // The shell must not buffer past a command's newline: `cat` inherits stdin
    // and should read the bytes that follow its command line, not have the shell
    // swallow them and then try to run them as commands.
    let out = run_with_input("cat\nPAYLOAD\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "PAYLOAD\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("command not found"),
        "stderr should be clean, was: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}
