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
fn non_interactive_command_stays_in_mesh_process_group() {
    let out = run_with_input(
        "sh -c 'test \"$(ps -o pgid= -p $$ | xargs)\" = \"$(ps -o pgid= -p $PPID | xargs)\"'\n",
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
fn semicolon_runs_commands_in_sequence() {
    let out = run_with_input("puts a; puts b\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\n");
}

#[test]
fn and_or_short_circuit_on_status() {
    // `&&` runs the next command only after success; `||` only after failure.
    let out = run_with_input(
        "true && puts ran-and\nfalse && puts skipped\nfalse || puts ran-or\ntrue || puts skipped\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran-and\nran-or\n");
}

#[test]
fn a_sequence_reports_the_last_commands_status() {
    // `true && false` short-circuits to false's status (1); a following `;`
    // still runs. The whole line's status is the last command actually run.
    assert_eq!(run_with_input("true && false\n").status.code(), Some(1));
    assert_eq!(run_with_input("false || true\n").status.code(), Some(0));
    // `exit` inside a sequence sees the previous command's status.
    assert_eq!(run_with_input("false; exit\n").status.code(), Some(1));
}

#[test]
fn a_quoted_separator_is_not_an_operator() {
    // A `;` inside quotes (or escaped) is a literal, not a command separator.
    let out = run_with_input("puts 'a;b'\nputs one\\;two\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a;b\none;two\n");
}

#[test]
fn bare_exit_uses_the_last_status() {
    // `exit` with no argument leaves the last command's status (POSIX), not 0.
    assert_eq!(run_with_input("false\nexit\n").status.code(), Some(1));
    assert_eq!(run_with_input("true\nexit\n").status.code(), Some(0));
    // An explicit argument still wins over the last status.
    assert_eq!(run_with_input("false\nexit 0\n").status.code(), Some(0));
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
fn tilde_preserves_home_bytes_including_trailing_slash() {
    // With a trailing slash in $HOME, `~/child` keeps the bytes verbatim
    // (`.../child` with the double slash), not a normalized single slash.
    let home = fresh_dir("tilde_slash");
    let mut home_with_slash = home.clone().into_os_string();
    home_with_slash.push("/");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .env("HOME", &home_with_slash)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"puts ~/child\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}//child\n", home.display())
    );
    let _ = std::fs::remove_dir_all(&home);
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
fn double_quotes_keep_spaces_in_one_argument() {
    let out = run_with_input("puts \"a b\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b\n");
}

#[test]
fn backslash_escapes_a_space() {
    let out = run_with_input("puts a\\ b\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b\n");
}

#[test]
fn double_quote_escapes_are_interpreted() {
    let out = run_with_input("puts \"x\\ty\\$5\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x\ty$5\n");
}

#[test]
fn empty_double_quotes_are_one_empty_argument() {
    let out = run_with_input("puts \"\" x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), " x\n");
}

#[test]
fn quoting_suppresses_glob_expansion() {
    let dir = fresh_dir("quote_glob");
    std::fs::write(dir.join("afile"), "").unwrap();
    // Unquoted `*` matches `afile`; quoted and escaped `*` stay literal.
    let out = run_with_input(&format!(
        "cd {}\nputs *\nputs '*'\nputs \\*\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "afile\n*\n*\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quoting_suppresses_tilde_expansion() {
    let home = fresh_dir("quote_tilde");
    let out = run_with_home("puts '~' \\~\n", &home);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "~ ~\n");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unterminated_quote_is_a_syntax_error_that_recovers() {
    // The bad line reports a syntax error; the shell keeps going.
    let out = run_with_input("puts 'oops\nputs ok\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax error"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn malformed_unicode_escape_is_a_syntax_error() {
    // Model B: an unknown/malformed escape is an error, not silently altered.
    let out = run_with_input("puts \"\\uZ\"\nputs ok\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax error"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn raw_strings_are_literal() {
    // r'…' takes no escapes — the home for regex source / paths.
    let out = run_with_input("puts r'\\d+\\.txt'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\\d+\\.txt\n");
}

#[test]
fn single_quotes_escape_in_model_b() {
    // `'a\tb'` is a real tab now (single quotes escape); `$x` stays literal.
    let out = run_with_input("puts 'a\\tb' '$x'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\tb $x\n");
}

#[test]
fn assignment_and_interpolation() {
    let out = run_with_input("x = hello\nputs $x\nn=42\nputs ${n}!\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n42!\n");
}

#[test]
fn interpolation_only_in_double_quotes() {
    let out = run_with_input("x = world\nputs \"hi $x\"\nputs 'hi $x'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi world\nhi $x\n");
}

#[test]
fn env_interpolation_reads_the_environment() {
    let home = fresh_dir("env_read");
    let out = run_with_home("puts $env.HOME\n", &home);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", home.display())
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn dotted_suffix_in_a_string_is_literal_not_member_access() {
    // Inside `"…"`, an unbraced `$x.txt` is `$x` + the literal `.txt` — member
    // access needs braces (`${x}` is fine too). `$env.HOME` access uses `${…}`
    // form when a literal dot must follow.
    let out = run_with_input("x = report\nputs \"$x.txt\"\nputs \"${x}.txt\"\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "report.txt\nreport.txt\n"
    );
}

#[test]
fn double_hyphen_name_is_not_a_valid_binding() {
    // `a--b` is not a kebab identifier (hyphens are interior, single), so it is
    // not an assignment target — the line is a command, and there is no such
    // command. The assignment target and the `$name` read agree on the rule.
    let out = run_with_input("a--b = v\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("a--b"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn unspaced_assignment_value_can_be_a_raw_string() {
    // `x=r'…'` must recognize the raw prefix at the value boundary, just like the
    // spaced `x = r'…'` form — storing the literal bytes, not `r` + a single-
    // quoted string (which would also choke on `\d` as an unknown escape).
    let out = run_with_input("x=r'\\d+'\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\\d+\n");
}

#[test]
fn raw_prefix_after_equals_matches_the_other_quotes() {
    // A raw string may begin a piece right after `=`, just like `'…'`/`"…"`
    // already do — so `k=r'v'`, `k='v'`, and `k="v"` all yield `k=v` as a plain
    // command argument (not an assignment).
    let out = run_with_input("puts option=r'abc'\nputs option='abc'\nputs option=\"abc\"\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "option=abc\noption=abc\noption=abc\n"
    );
}

#[test]
fn assignment_to_reserved_env_name_is_rejected() {
    // `env` is the environment namespace; a plain `env` binding would be shadowed
    // by `$env.KEY` reads and could never be read back, so it is rejected loudly.
    let out = run_with_input("env=hello\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved name"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn unterminated_braced_interpolation_is_a_syntax_error() {
    // `${` signals interpolation intent, so a missing `}` (or a malformed name
    // inside) is a loud syntax error, not silent literal text — a literal `$`
    // in a string is `\$`. An unbraced `$5` stays a literal `$5`.
    let out = run_with_input("x = abc\nputs \"${x\"\nputs \"$5\"\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax error"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "$5\n");
}

#[test]
fn leading_underscore_is_not_a_variable_name() {
    // A name starts with a letter; `_` is reserved as the discard pattern, so
    // `_`/`_x` are not bindable (the line is a command, which isn't found) and
    // `$_` is a literal. An interior underscore (`a_b`) is still a valid name.
    let out = run_with_input("_ = secret\na_b = ok\nputs $a_b\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("command not found: _"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\nafter\n");
}

#[test]
fn unbound_variable_is_a_loud_error_that_recovers() {
    let out = run_with_input("puts $nope\nputs ok\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unbound variable"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn interpolated_value_is_not_re_globbed() {
    // A `$x` holding `*` is one literal value — no word splitting or globbing.
    let dir = fresh_dir("interp_glob");
    std::fs::write(dir.join("afile"), "").unwrap();
    let out = run_with_input(&format!("cd {}\nx = '*'\nputs $x\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "*\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trailing_line_continuation_adds_no_empty_argument() {
    // `puts a \<newline>` must yield just `a`, not `a` plus an empty argument.
    let out = run_with_input("puts a \\\n\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n");
}

#[test]
fn quoted_fragment_cannot_complete_a_glob_class() {
    // `['*'` is a literal `[*`, not the pattern `[[*]` — escaping the quoted `*`
    // must not close the unquoted `[`.
    let dir = fresh_dir("glob_class");
    let out = run_with_input(&format!("cd {}\nputs ['*'\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[*\n");
    let _ = std::fs::remove_dir_all(&dir);
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

#[test]
fn background_interactive_startup_stops_until_foregrounded() {
    // Run the PTY choreography in an isolated session so this test cannot
    // change the test runner's controlling terminal or process group.
    let harness = unsafe { libc::fork() };
    assert!(
        harness >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if harness == 0 {
        unsafe { libc::_exit(background_startup_harness()) };
    }

    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "PTY harness failed with status {status:#x}"
    );
}

fn background_startup_harness() -> i32 {
    use std::ffi::CString;
    use std::os::fd::RawFd;

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, libc::TIOCSCTTY, 0) } < 0
    {
        return 10;
    }
    // Closing the last PTY descriptor can hang up this isolated session while
    // the harness is reporting success; that is unrelated to mesh's behavior.
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let harness_group = unsafe { libc::getpgrp() };
    if unsafe { libc::tcsetpgrp(slave, harness_group) } < 0 {
        return 11;
    }

    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return 12;
    }
    if mesh == 0 {
        unsafe {
            libc::setpgid(0, 0);
            libc::dup2(slave, libc::STDIN_FILENO);
            libc::dup2(slave, libc::STDOUT_FILENO);
            libc::dup2(slave, libc::STDERR_FILENO);
            libc::close(master);
            libc::close(slave);
        }
        let path = CString::new(env!("CARGO_BIN_EXE_mesh")).unwrap();
        let arg0 = CString::new("mesh").unwrap();
        unsafe {
            libc::execl(
                path.as_ptr(),
                arg0.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
            libc::_exit(127);
        }
    }
    unsafe { libc::close(slave) };

    let mut status = 0;
    if unsafe { libc::waitpid(mesh, &mut status, libc::WUNTRACED) } != mesh
        || !libc::WIFSTOPPED(status)
        || libc::WSTOPSIG(status) != libc::SIGTTIN
    {
        return 13;
    }
    if unsafe { libc::tcsetpgrp(master, mesh) } < 0
        || unsafe { libc::kill(mesh, libc::SIGCONT) } < 0
    {
        return 14;
    }
    // Wait until reedline has initialized (which may flush pending input).
    if !pty_wait_for_prompt(master) {
        return 17;
    }
    if unsafe { libc::write(master, b"\x04".as_ptr().cast(), 1) } != 1 {
        return 14;
    }
    if unsafe { libc::waitpid(mesh, &mut status, 0) } != mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 15;
    }
    unsafe { libc::close(master) };
    0
}

/// Act as the small piece of terminal-emulator behavior reedline needs while
/// waiting for its prompt.
fn pty_wait_for_prompt(master: std::os::fd::RawFd) -> bool {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut prompt = Vec::new();
    for _ in 0..8 {
        if unsafe { libc::poll(&mut ready, 1, 2_000) } <= 0 {
            return false;
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return false;
        }
        prompt.extend_from_slice(&chunk[..count as usize]);
        if prompt.windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
        if prompt.windows(5).any(|part| part == b"mesh$") {
            break;
        }
    }
    prompt.windows(5).any(|part| part == b"mesh$")
}

#[test]
fn a_pipe_connects_two_commands() {
    let out = run_with_input("printf 'a\\nb\\nc\\n' | grep b\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b\n");
}

#[test]
fn a_three_stage_pipeline_works() {
    let out = run_with_input("printf '3\\n1\\n2\\n' | sort | head -1\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn output_redirection_writes_a_file_and_input_reads_it() {
    let dir = fresh_dir("redir_io");
    let out = run_with_input(&format!("cd {}\necho hello > f\ncat < f\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
    assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "hello\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_redirection_adds_to_a_file() {
    let dir = fresh_dir("redir_append");
    let out = run_with_input(&format!(
        "cd {}\necho one > f\necho two >> f\ncat f\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\ntwo\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pipeline_status_is_pipefail() {
    // A failing upstream stage fails the pipeline even if the last stage is fine.
    assert_eq!(run_with_input("false | true\n").status.code(), Some(1));
    assert_eq!(run_with_input("true | false\n").status.code(), Some(1));
    assert_eq!(run_with_input("true | true\n").status.code(), Some(0));
}

#[test]
fn upstream_sigpipe_does_not_fail_the_pipeline() {
    // `yes` is SIGPIPE-killed once `head` closes the pipe, but that is not a
    // failure — the pipeline succeeds.
    let out = run_with_input("yes | head -1\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "y\n");
}

#[test]
fn a_quoted_pipe_is_a_literal_not_an_operator() {
    let out = run_with_input("echo 'a|b'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a|b\n");
}

#[test]
fn a_builtin_in_a_pipeline_is_rejected_for_now() {
    let out = run_with_input("puts hi | cat\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not supported in a pipeline"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_redirected_builtin_is_rejected_for_now() {
    let dir = fresh_dir("redir_builtin");
    let out = run_with_input(&format!("cd {}\npwd > f\nputs after\n", dir.display()));
    assert!(String::from_utf8_lossy(&out.stderr).contains("redirection of a builtin"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirect_with_no_target_is_a_syntax_error_that_recovers() {
    let out = run_with_input("echo hi >\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("redirection needs a target"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn an_empty_pipeline_stage_is_a_syntax_error_that_recovers() {
    let out = run_with_input("echo hi |\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("empty command in a pipeline"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_redirected_producer_gives_the_next_stage_eof() {
    // `printf … > f | cat` sends printf's output to the file, so `cat` must read
    // EOF (an empty pipe), not inherit the shell's stdin and swallow the next
    // script line. The following `echo` must still run.
    let dir = fresh_dir("redir_producer");
    let out = run_with_input(&format!(
        "cd {}\nprintf x > f | cat\necho sentinel\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sentinel\n");
    assert_eq!(std::fs::read_to_string(dir.join("f")).unwrap(), "x");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn downstream_stages_run_after_an_upstream_spawn_failure() {
    // A not-found producer must not stop the rest of the pipeline: `echo` still
    // runs (reading EOF), and pipefail keeps the 127.
    let out = run_with_input("nosuchcmd | echo after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("command not found"));
    assert_eq!(out.status.code(), Some(127));
}

#[test]
fn a_sigpipe_in_the_final_stage_still_counts() {
    // The SIGPIPE exemption is only for a stage feeding a pipe. The last stage
    // has no downstream reader, so a SIGPIPE there is a real failure (141).
    let out = run_with_input("true | sh -c 'kill -PIPE $$'\n");
    assert_eq!(out.status.code(), Some(141));
}

#[test]
fn redirections_apply_in_source_order() {
    // `cat > out < missing` opens (creates/truncates) `out` first, then fails on
    // the missing input — so `out` exists even though the command failed.
    let dir = fresh_dir("redir_order");
    let out = run_with_input(&format!(
        "cd {}\ncat > out < missing\nputs after\n",
        dir.display()
    ));
    assert!(
        dir.join("out").exists(),
        "out should have been created first"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_descriptor_redirect_is_rejected_for_now() {
    // `2>err` and `&>f` are deferred descriptor redirects — rejected loudly, not
    // silently reinterpreted as a stdout redirect with a stray `2`/`&` argument.
    let out = run_with_input("echo hello 2>err\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("descriptor redirection"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let amp = run_with_input("echo hello &>f\nputs after\n");
    assert!(String::from_utf8_lossy(&amp.stderr).contains("descriptor redirection"));
    // `&>` attached to a preceding argument (`hello&>f`) is still rejected.
    let attached = run_with_input("echo hello&>f\nputs after\n");
    assert!(String::from_utf8_lossy(&attached.stderr).contains("descriptor redirection"));
    // The fd-duplication form with `&` after the operator (`>&2`, `<&0`) too.
    let dup = run_with_input("echo hi >&2\nputs after\n");
    assert!(String::from_utf8_lossy(&dup.stderr).contains("descriptor redirection"));
    let dupin = run_with_input("cat <&0\nputs after\n");
    assert!(String::from_utf8_lossy(&dupin.stderr).contains("descriptor redirection"));
    // But an escaped `\&` is a literal, so `hi\&>f` is a normal redirect.
    let dir = fresh_dir("redir_escaped_amp");
    let esc = run_with_input(&format!("cd {}\necho hi\\&>f\ncat f\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&esc.stdout), "hi&\n");
    let _ = std::fs::remove_dir_all(&dir);
    // And a bare fd needs *only* digits abutting the operator: an empty quote
    // (`""2>f`) or an escaped digit (`\2>f`) is a normal argument + redirect.
    let dir2 = fresh_dir("redir_empty_quote_fd");
    let eq = run_with_input(&format!("cd {}\necho \"\"2>f\ncat f\n", dir2.display()));
    assert_eq!(String::from_utf8_lossy(&eq.stdout), "2\n");
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn a_fifo_redirect_in_a_pipeline_does_not_deadlock() {
    // Two stages of one pipeline open the same FIFO (one for read, one for
    // write). The redirections must open concurrently, or the parent deadlocks
    // opening the reader before the writer is spawned. Guarded by a timeout so a
    // regression fails the test instead of hanging CI.
    let dir = fresh_dir("fifo_pipe");
    let fifo = dir.join("f");
    let made = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        let _ = std::fs::remove_dir_all(&dir);
        return; // mkfifo unavailable — skip
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"cat < f | echo hi > f\nputs done\n")
        .expect("write stdin");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("mesh deadlocked on a FIFO redirect in a pipeline");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("wait");
    assert!(String::from_utf8_lossy(&out.stdout).contains("done"));
    let _ = std::fs::remove_dir_all(&dir);
}
