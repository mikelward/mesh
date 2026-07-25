//! End-to-end tests that drive the built `mesh` binary.
//!
//! No test-harness crates: Cargo exposes the binary path as `CARGO_BIN_EXE_mesh`
//! to integration tests, so std is enough. Input is piped on stdin (making the
//! shell non-interactive, so no prompt is written), and we assert on stdout,
//! stderr, and the exit code.

use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

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

fn isolated_config_home() -> &'static Path {
    static CONFIG_HOME: OnceLock<PathBuf> = OnceLock::new();
    CONFIG_HOME.get_or_init(|| fresh_dir("default_config"))
}

fn mesh_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mesh"));
    command.env("XDG_CONFIG_HOME", isolated_config_home());
    command
}

struct MeshExec {
    path: std::ffi::CString,
    _arguments: [std::ffi::CString; 1],
    argv: [*const libc::c_char; 2],
    _environment: Vec<std::ffi::CString>,
    envp: Vec<*const libc::c_char>,
}

impl MeshExec {
    fn new(config_home: &Path) -> Self {
        use std::ffi::CString;

        let path = CString::new(env!("CARGO_BIN_EXE_mesh")).unwrap();
        let arguments = [CString::new("mesh").unwrap()];
        let argv = [arguments[0].as_ptr(), std::ptr::null()];

        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(name, _)| name != "XDG_CONFIG_HOME")
            .map(|(name, value)| {
                let mut entry = name.into_encoded_bytes();
                entry.push(b'=');
                entry.extend(value.into_encoded_bytes());
                CString::new(entry).unwrap()
            })
            .collect();
        let mut config = b"XDG_CONFIG_HOME=".to_vec();
        config.extend(config_home.as_os_str().as_bytes());
        environment.push(CString::new(config).unwrap());

        let mut envp: Vec<_> = environment.iter().map(|entry| entry.as_ptr()).collect();
        envp.push(std::ptr::null());

        Self {
            path,
            _arguments: arguments,
            argv,
            _environment: environment,
            envp,
        }
    }
}

fn exec_mesh(exec: &MeshExec) -> i32 {
    unsafe {
        libc::execve(exec.path.as_ptr(), exec.argv.as_ptr(), exec.envp.as_ptr());
    }
    127
}

/// Run mesh with `HOME` set to `home` (for tilde tests).
fn run_with_home(input: &str, home: &Path) -> Output {
    let mut child = mesh_command()
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

fn run_with_config(input: &str, config_home: &Path, args: &[&str]) -> Output {
    let mut child = mesh_command()
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
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

#[test]
fn non_interactive_shell_sources_env_config() {
    let config = fresh_dir("env_config");
    let mesh = config.join("mesh");
    std::fs::create_dir(&mesh).unwrap();
    std::fs::write(mesh.join("env.mesh"), "greeting = from-env\n").unwrap();

    let out = run_with_config("puts $greeting\n", &config, &[]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from-env\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn login_config_runs_in_order_and_logout_runs_on_exit() {
    let config = fresh_dir("login_config");
    let mesh = config.join("mesh");
    std::fs::create_dir(&mesh).unwrap();
    std::fs::write(mesh.join("env.mesh"), "value = env\n").unwrap();
    std::fs::write(mesh.join("login.mesh"), "puts $value\nvalue = login\n").unwrap();
    std::fs::write(mesh.join("logout.mesh"), "puts logout-$value\n").unwrap();

    let out = run_with_config("puts $value\nexit 7\n", &config, &["--login"]);

    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "env\nlogin\nlogout-login\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn non_interactive_shell_does_not_source_rc_config() {
    let config = fresh_dir("noninteractive_rc");
    let mesh = config.join("mesh");
    std::fs::create_dir(&mesh).unwrap();
    std::fs::write(mesh.join("rc.mesh"), "puts wrong\n").unwrap();

    let out = run_with_config("puts right\n", &config, &[]);

    assert_eq!(String::from_utf8_lossy(&out.stdout), "right\n");
    assert!(out.stderr.is_empty());
}

fn run_with_bytes(input: &[u8]) -> Output {
    let mut child = mesh_command()
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
fn expression_parse_errors_recover_before_the_next_command() {
    let out = run_with_input("result = 1 < 2 < 3\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("comparisons cannot be chained"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn assignment_parse_errors_are_authoritative() {
    let out = run_with_input("result = 1 + )\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("syntax error"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn incomplete_assignment_at_eof_is_a_syntax_error() {
    let out = run_with_input("result = 1 +");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("syntax error: unexpected end of input"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
fn non_interactive_child_preserves_an_ignored_sigint() {
    let mut child = mesh_command();
    child
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        child.pre_exec(|| {
            if libc::signal(libc::SIGINT, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = child.spawn().expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"sh -c 'kill -INT $$; echo survived'\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "survived\n");
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
fn if_runs_the_branch_selected_by_command_status() {
    let out = run_with_input(
        "if true {\n  puts then\n} else {\n  puts wrong\n}\n\
         if false { puts wrong } else { puts else }\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "then\nelse\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn if_chains_else_if_and_propagates_control_flow() {
    let out = run_with_input(
        "if false { puts wrong } else if true { puts nested }\n\
         func choose() {\n\
           if true { return 7 }\n\
           puts wrong\n\
         }\n\
         choose\n",
    );
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nested\n");
}

#[test]
fn if_expression_assigns_the_selected_typed_value() {
    let out = run_with_input(
        "word = if true { \"chosen value\" } else { wrong }\n\
         items = if false { [wrong] } else { [one \"two three\"] }\n\
         missing = if false { wrong }\n\
         puts $word\n\
         puts ...$items\n\
         puts \"<$missing>\"\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "chosen value\none two three\n<>\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn general_lists_preserve_nesting_and_spread_one_level() {
    let out = run_with_input(
        "inner = [two three]\n\
         nested = [one $inner four]\n\
         puts ...$nested[1]\n\
         flat = [zero ...$inner four]\n\
         flat += [five six]\n\
         puts ...$flat[1..=-1]\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "two three\ntwo three four five six\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn indexed_nested_lists_remain_typed_in_value_contexts() {
    let out = run_with_input(
        "nested = [zero [one two] three]\n\
         copy = $nested[1]\n\
         puts ...$copy\n\
         func show(value) { puts ...$value }\n\
         show $nested[1]\n\
         wrapped = [$nested[1]]\n\
         puts ...$wrapped[0]\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "one two\none two\none two\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn scalar_glob_brackets_do_not_delimit_nested_lists() {
    let out = run_with_input(
        "outer = [[a[b c]]\n\
         puts ...$outer[0]\n\
         outer = [[a]b c]]\n\
         puts ...$outer[0]\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a[b c\na]b c\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_nested_list_cannot_cross_the_command_boundary_implicitly() {
    let out = run_with_input("xs = [[one two]]\nputs ...$xs\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("nested list element cannot be a command argument")
    );
}

#[test]
fn empty_command_positions_are_syntax_errors() {
    for script in [
        "; puts no\n",
        "puts no ;; puts no\n",
        "true &&\n",
        "false ||\n",
    ] {
        let out = run_with_input(script);
        assert_eq!(out.status.code(), Some(2), "{script:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{script:?}"
        );
        assert!(out.stdout.is_empty(), "{script:?}");
    }
}

#[test]
fn one_trailing_semicolon_is_allowed() {
    let out = run_with_input("puts yes;\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "yes\n");
    assert!(out.stderr.is_empty());
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
    let mut child = mesh_command()
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
fn list_literal_preserves_arity_and_spreads_into_arguments() {
    let out = run_with_input("xs = [a 'b c' d]\nputs ...$xs\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b c d\n");
}

#[test]
fn list_literal_accepts_a_spread_immediately_before_the_closing_bracket() {
    let out = run_with_input(
        "xs = [second third]\nys = [first ...$xs]\nputs ...$ys\nys = [...$xs]\nputs ...$ys\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "first second third\nsecond third\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn empty_list_spreads_to_no_arguments() {
    let out = run_with_input("xs = []\nputs before ...$xs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before after\n");
}

#[test]
fn list_literal_preserves_quoted_empty_elements() {
    let out = run_with_input(
        "xs = [\"\" a]\nprintf '<%s>\\n' ...$xs\nxs = [\"\"]\nprintf '<%s>\\n' ...$xs\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<>\n<a>\n<>\n");
}

#[test]
fn append_assignment_concatenates_strings_and_grows_lists() {
    let out = run_with_input(
        "greeting = hi\ngreeting += ' there'\nputs $greeting\nxs = [a b]\nxs += c\nxs += [d e]\nmore = [f g]\nxs += $more\nputs ...$xs\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi there\na b c d e f g\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn append_assignment_preserves_list_slices() {
    let out = run_with_input(
        "xs = [a b]\nmore = [c d e]\nxs += $more[1..]\nputs ...$xs\nxs += $more[9..]\nputs ...$xs\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b d e\na b d e\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn unspaced_append_assignment_and_type_errors_recover() {
    let out = run_with_input(
        "x=one\nx+=two\nputs $x\nxs=[a]\nxs+=b\nputs ...$xs\nx += [bad]\nmissing += value\nputs recovered\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "onetwo\na b\nrecovered\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot append a list to a string"));
    assert!(stderr.contains("missing: unbound variable"));
}

#[test]
fn list_requires_explicit_spread_in_command_arguments() {
    let out = run_with_input("xs = [a b]\nputs $xs\nputs recovered\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("list value needs `...`"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "recovered\n");
}

#[test]
fn list_indexing_is_zero_based_and_supports_negative_indices() {
    let out = run_with_input("xs = [a 'b c' d]\nputs $xs[0] $xs[-1] ${xs[1]}!\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a d b c!\n");
}

#[test]
fn list_slices_are_clamped_and_require_spread() {
    let out = run_with_input(
        "xs = [a b c d]\nputs ...$xs[1..3]\nputs ...$xs[..=1]\nputs ...$xs[-2..]\nputs ...$xs[..=-1]\nputs ...$xs[..=9223372036854775807]\nputs before ...$xs[9..] after\nputs before ...$xs[..=-5] after\nputs before ...$xs[..=-4] after\nputs $xs[1..2]\ns = text\nputs $s[1..]\nputs $missing[1..]\nputs recovered\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "b c\na b\nc d\na b c d\na b c d\nbefore after\nbefore after\nbefore a after\nrecovered\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("list value needs `...`"));
    assert!(stderr.contains("cannot index a string value"));
    assert!(stderr.contains("missing: unbound variable"));
}

#[test]
fn assignment_copies_whole_lists_and_list_slices() {
    let out = run_with_input(
        "xs = [a b c d]\nys = $xs\nzs=$xs[1..=2]\nxs += e\nputs ...$ys\nputs ...$zs\nputs ...$xs\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a b c d\nb c\na b c d e\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn quoted_interpolations_do_not_copy_lists_in_assignments() {
    let out = run_with_input("xs = [a b c d]\nys = \"$xs\"\nzs = \"${xs[1..]}\"\nputs recovered\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "recovered\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr)
            .matches("list value needs `...`")
            .count(),
        2
    );
}

#[test]
fn invalid_list_index_fails_loudly_and_recovers() {
    let out = run_with_input(
        "xs = [a b]\nputs $xs[2]\nputs $xs[-3]\nx = text\nputs $x[0]\nputs recovered\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("list index out of range").count(), 2);
    assert!(stderr.contains("cannot index a string value"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "recovered\n");
}

#[test]
fn interpolation_only_in_double_quotes() {
    let out = run_with_input("x = world\nputs \"hi $x\"\nputs 'hi $x'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi world\nhi $x\n");
}

#[test]
fn env_interpolation_reads_the_environment() {
    let home = fresh_dir("env_read");
    let out = run_with_home(
        "puts $env.HOME\nputs \"$env.HOME\"\nputs \"${env.HOME}\"\n",
        &home,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{0}\n{0}\n{0}\n", home.display())
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn braced_variable_delimits_a_literal_dotted_suffix() {
    let out = run_with_input("x = report\nputs \"${x}.txt\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "report.txt\n");
}

#[test]
fn list_indexing_works_inside_double_quotes() {
    let out = run_with_input("xs = [first last]\nputs \"$xs[0] ${xs[-1]}\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "first last\n");
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
fn quoted_hyphen_stays_literal_inside_a_glob_class() {
    let dir = fresh_dir("glob_quoted_hyphen");
    for name in ["-", "a", "m", "z"] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    let out = run_with_input(&format!("cd {}\nputs [a'-'z]\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "- a z\n");
    let _ = std::fs::remove_dir_all(&dir);
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
    let mut child = mesh_command()
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
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(
        harness >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if harness == 0 {
        unsafe { libc::_exit(background_startup_harness(&exec)) };
    }

    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "PTY harness failed with status {status:#x}"
    );
}

#[test]
fn new_foreground_job_does_not_receive_sigcont() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(sigcont_harness(&exec)) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "PTY harness failed with status {status:#x}"
    );
}

#[test]
fn spawn_failure_returns_terminal_to_interactive_shell() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(spawn_failure_harness(&exec)) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "PTY harness failed with status {status:#x}"
    );
}

fn spawn_failure_harness(exec: &MeshExec) -> i32 {
    let mut master = -1;
    let mut slave = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
    {
        return 30;
    }
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return 31;
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
        unsafe { libc::_exit(exec_mesh(exec)) };
    }
    // Set the group from both sides of fork so tcsetpgrp cannot race the child.
    if unsafe { libc::setpgid(mesh, mesh) } < 0 && unsafe { libc::getpgid(mesh) } != mesh {
        return 39;
    }
    unsafe { libc::close(slave) };
    if unsafe { libc::tcsetpgrp(master, mesh) } < 0 || !pty_wait_for_prompt(master) {
        return 32;
    }
    let missing = b"mesh-command-that-does-not-exist\n";
    if unsafe { libc::write(master, missing.as_ptr().cast(), missing.len()) }
        != missing.len() as isize
        || pty_read_until_any_prompt(master).is_none()
    {
        return 33;
    }
    let command = b"puts recovered\n";
    if unsafe { libc::write(master, command.as_ptr().cast(), command.len()) }
        != command.len() as isize
    {
        return 34;
    }
    let output = match pty_read_until_prompt(master) {
        Some(output) => output,
        None => return 35,
    };
    if !output.windows(11).any(|part| part == b"recovered\r\n") {
        return 36;
    }
    if unsafe { libc::write(master, b"exit\n".as_ptr().cast(), 5) } != 5 {
        return 37;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(mesh, &mut status, 0) } != mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 38;
    }
    unsafe { libc::close(master) };
    0
}

fn sigcont_harness(exec: &MeshExec) -> i32 {
    let mut master = -1;
    let mut slave = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
    {
        return 20;
    }
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return 21;
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
        unsafe { libc::_exit(exec_mesh(exec)) };
    }
    // Set the group from both sides of fork so tcsetpgrp cannot race the child.
    if unsafe { libc::setpgid(mesh, mesh) } < 0 && unsafe { libc::getpgid(mesh) } != mesh {
        return 28;
    }
    unsafe { libc::close(slave) };
    if unsafe { libc::tcsetpgrp(master, mesh) } < 0 || !pty_wait_for_prompt(master) {
        return 22;
    }

    // Extra stages give the first process ample time to install its handler
    // before mesh finishes launching the group. An unconditional group-wide
    // SIGCONT after launch therefore makes "unsolicited" observable.
    let mut command = String::from("sh -c 'trap \"echo unsolicited\" CONT; sleep 0.2; echo done'");
    for _ in 0..24 {
        command.push_str(" | cat");
    }
    command.push('\n');
    if unsafe { libc::write(master, command.as_ptr().cast(), command.len()) }
        != command.len() as isize
    {
        return 23;
    }
    let output = match pty_read_until_prompt(master) {
        Some(output) => output,
        None => return 24,
    };
    if output.windows(13).any(|part| part == b"unsolicited\r\n") {
        return 25;
    }
    if unsafe { libc::write(master, b"exit\n".as_ptr().cast(), 5) } != 5 {
        return 26;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(mesh, &mut status, 0) } != mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 27;
    }
    unsafe { libc::close(master) };
    0
}

fn background_startup_harness(exec: &MeshExec) -> i32 {
    use std::os::fd::RawFd;

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
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
        unsafe { libc::_exit(exec_mesh(exec)) };
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

#[test]
fn nested_background_work_joins_the_stage_group() {
    // A backgrounded function that backgrounds something itself must not start a
    // process group of its own: the shell tracks the stage's group, and a nested
    // group would escape it — and print a second, conflicting `[1]` notice from
    // inside the fork.
    //
    // The nested command still outlives the job, as it does in bash: the stage
    // returns as soon as it has started it, so the job completes and is reaped
    // while the child runs on. `&` means "do not wait" inside a fork too. What
    // this pins down is the group and the single notice, not the lifetime.
    let dir = fresh_dir("nested_background");
    let out = run_with_input(&format!(
        "cd {}\nfunc f() {{ sh -c 'ps -o pgid= -p $$ > pgid' & }}\nf &\nsleep 0.4\n",
        dir.display()
    ));
    let notices = String::from_utf8_lossy(&out.stderr);
    let started: Vec<&str> = notices
        .lines()
        .filter(|line| line.starts_with("[1] "))
        .collect();
    assert_eq!(started.len(), 1, "one job notice, got {started:?}");
    let tracked = started[0].trim_start_matches("[1] ").trim();
    let nested = std::fs::read_to_string(dir.join("pgid")).expect("nested command ran");
    assert_eq!(
        nested.trim(),
        tracked,
        "nested background work should share the stage's group"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stage_does_not_leak_its_pipe_to_nested_background_work() {
    // The fork duplicates its pipe onto fd 1 and must then close the original.
    // It leaves via `_exit`, so nothing else ever will — and a second open write
    // end is inherited by everything the stage starts, keeping the reader from
    // seeing EOF long after the stage itself has finished.
    //
    // The nested command's own streams go to `/dev/null`, so it holds no
    // legitimate claim on anything: `cat` should report EOF as soon as the stage
    // exits, and the script reach `puts after`. Both streams have to be
    // redirected, or the background command keeps *this harness's* captured
    // stdout/stderr open and the wait would prove nothing about the pipe.
    let timed = |script: &str| {
        let start = std::time::Instant::now();
        let out = run_with_input(script);
        (out, start.elapsed())
    };
    let (redirected, elapsed) =
        timed("func f() { sleep 5 > /dev/null 2> /dev/null &\nputs hi }\nf | cat\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&redirected.stdout),
        "hi\nafter\n",
        "{:?}",
        redirected.stderr
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the reader waited on a leaked descriptor: {elapsed:?}"
    );

    // The contrast, which is *not* a leak: with stdout left alone the nested
    // command inherits the stage's, so it holds the pipe legitimately and the
    // reader does wait for it. bash agrees — `bash -c 'set -m; f(){ sleep 6 & echo
    // hi; }; f | cat'` blocks for the sleep, and returns at once as soon as the
    // sleep's stdout is redirected away.
    let (inherited, waited) =
        timed("func f() { sleep 0.5 2> /dev/null &\nputs hi }\nf | cat\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&inherited.stdout),
        "hi\nafter\n",
        "{:?}",
        inherited.stderr
    );
    assert!(
        waited >= std::time::Duration::from_millis(400),
        "a nested command that inherits the pipe should hold it: {waited:?}"
    );
}

#[test]
fn a_backgrounded_function_leaves_the_terminal_with_the_shell() {
    // `f &` forks the function, and the body's own `sleep` must not be treated as
    // a foreground job: the fork is not the interactive shell. If it were, the
    // nested command would take a new process group and `tcsetpgrp` the terminal
    // away from mesh, and the prompt would stop accepting input.
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(
        harness >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if harness == 0 {
        unsafe { libc::_exit(background_function_terminal_harness(&exec)) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "PTY harness failed with status {status:#x}"
    );
}

fn background_function_terminal_harness(exec: &MeshExec) -> i32 {
    use std::os::fd::RawFd;

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
    {
        return 10;
    }
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
        unsafe { libc::_exit(exec_mesh(exec)) };
    }
    unsafe { libc::close(slave) };

    // mesh stops on SIGTTIN reading from a terminal it does not own; hand it over.
    let mut status = 0;
    if unsafe { libc::waitpid(mesh, &mut status, libc::WUNTRACED) } != mesh
        || !libc::WIFSTOPPED(status)
        || unsafe { libc::tcsetpgrp(master, mesh) } < 0
        || unsafe { libc::kill(mesh, libc::SIGCONT) } < 0
    {
        return 13;
    }
    if !pty_wait_for_prompt(master) {
        return 14;
    }
    // The body runs a real external command and *then* reports. Checking right
    // after `f &` would race the fork: the prompt can return before the stage has
    // spawned anything, so the window where a nested command could take the
    // terminal would not have been entered yet.
    for line in ["func f() { sleep 0.3; puts inner-ran }\n", "f &\n"] {
        if unsafe { libc::write(master, line.as_ptr().cast(), line.len()) } != line.len() as isize {
            return 15;
        }
        if !pty_wait_for_prompt(master) {
            return 16;
        }
    }
    // Wait for evidence the nested `sleep` actually ran to completion, so the
    // whole window has been crossed before the terminal is inspected.
    if !pty_wait_for_marker(master, b"inner-ran") {
        return 22;
    }
    // The shell, not the backgrounded job's `sleep`, must still own the terminal.
    if unsafe { libc::tcgetpgrp(master) } != mesh {
        return 17;
    }
    // And it must still be able to run the next command.
    let alive = "puts alive\n";
    if unsafe { libc::write(master, alive.as_ptr().cast(), alive.len()) } != alive.len() as isize {
        return 18;
    }
    let Some(echoed) = pty_read_until_prompt(master) else {
        return 19;
    };
    if !echoed.windows(5).any(|part| part == b"alive") {
        return 20;
    }
    if unsafe { libc::write(master, b"\x04".as_ptr().cast(), 1) } != 1 {
        return 21;
    }
    unsafe { libc::waitpid(mesh, &mut status, 0) };
    unsafe { libc::close(master) };
    0
}

/// Act as the small piece of terminal-emulator behavior reedline needs while
/// waiting for its prompt.
fn pty_wait_for_prompt(master: std::os::fd::RawFd) -> bool {
    pty_read_until_prompt(master).is_some()
}

fn pty_read_until_prompt(master: std::os::fd::RawFd) -> Option<Vec<u8>> {
    let prompt = pty_read_until_any_prompt(master)?;
    prompt
        .windows(5)
        .any(|part| part == b"mesh$")
        .then_some(prompt)
}

/// Read from the PTY until `marker` appears, answering cursor-position queries so
/// reedline keeps going. Used to wait for evidence that a backgrounded body has
/// actually run, rather than checking the moment the prompt returns.
fn pty_wait_for_marker(master: std::os::fd::RawFd, marker: &[u8]) -> bool {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut seen = Vec::new();
    for _ in 0..40 {
        if seen.windows(marker.len()).any(|part| part == marker) {
            return true;
        }
        if unsafe { libc::poll(&mut ready, 1, 2_000) } <= 0 {
            return false;
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return false;
        }
        seen.extend_from_slice(&chunk[..count as usize]);
        if seen.windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
    }
    seen.windows(marker.len()).any(|part| part == marker)
}

fn pty_read_until_any_prompt(master: std::os::fd::RawFd) -> Option<Vec<u8>> {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut prompt = Vec::new();
    for _ in 0..8 {
        let found = prompt
            .windows(5)
            .any(|part| part == b"mesh$" || part == b"mesh!");
        let timeout = if found { 50 } else { 2_000 };
        if unsafe { libc::poll(&mut ready, 1, timeout) } <= 0 {
            return found.then_some(prompt);
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return None;
        }
        prompt.extend_from_slice(&chunk[..count as usize]);
        if prompt.windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
    }
    prompt
        .windows(5)
        .any(|part| part == b"mesh$" || part == b"mesh!")
        .then_some(prompt)
}

#[test]
fn a_pipe_connects_two_commands() {
    let out = run_with_input("printf 'a\\nb\\nc\\n' | grep b\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b\n");
}

#[test]
fn parser_incomplete_pipeline_continues_on_the_next_line() {
    let out = run_with_input("printf 'complete\\n' |\ncat\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "complete\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_three_stage_pipeline_works() {
    let out = run_with_input("printf '3\\n1\\n2\\n' | sort | head -1\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn a_pipeline_can_run_in_the_background() {
    let dir = fresh_dir("background_pipeline");
    let out = run_with_input(&format!(
        "sh -c 'sleep 0.05; echo background > {0}/result' | cat & puts foreground\nsleep 0.15\ncat {0}/result\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "foreground\nbackground\n"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("[1]"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_command_does_not_consume_shell_input() {
    let out = run_with_input("cat & puts after\nsleep 0.05\njobs\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("[1]"));
}

#[test]
fn background_pipeline_retains_statuses_reaped_on_earlier_prompts() {
    let out = run_with_input("sh -c 'exit 7' | sleep 0.2 &\nsleep 0.05\njobs\nsleep 0.25\njobs\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Done (7)"));
}

#[test]
fn foreground_pipeline_retains_statuses_reaped_on_earlier_prompts() {
    let out = run_with_input("sh -c 'exit 7' | sleep 0.2 &\nsleep 0.05\njobs\nfg\nexit\n");
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn quoted_and_escaped_ampersands_are_literal() {
    let out = run_with_input("echo 'a&b' c\\&d\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a&b c&d\n");
}

#[test]
fn an_empty_background_command_is_a_syntax_error() {
    let out = run_with_input("&\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs a command"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
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
fn a_builtin_runs_as_a_pipeline_stage() {
    let out = run_with_input("puts hi | cat\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi\nafter\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And as the receiving end, where its own output still reaches the terminal.
    let out = run_with_input("echo ignored | puts read-nothing\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "read-nothing\n");
}

#[test]
fn a_redirected_builtin_writes_to_the_target() {
    // A builtin runs inside the shell, so — like a function — its `>` applies to
    // the shell's own descriptors for the duration of the call.
    let dir = fresh_dir("redir_builtin");
    let out = run_with_input(&format!("cd {}\npwd > f\nputs after\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    let written = std::fs::read_to_string(dir.join("f")).expect("pwd wrote the file");
    assert_eq!(
        written.trim_end(),
        dir.canonicalize().unwrap().display().to_string()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirected_builtin_survives_a_failing_write() {
    // A redirected builtin writes through the shell's own descriptors, so a write
    // error reaches the shell. It must report a failing status, not panic the way
    // `println!` does — the shell has to still be there for the next statement.
    let out = run_with_input("prompt > /dev/full\njobs > /dev/full\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after\n",
        "{:?}",
        out.stdout
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mesh: prompt:"), "{stderr:?}");
    assert!(!stderr.contains("panicked"), "{stderr:?}");

    // The status is the builtin's failure, not the shell's death.
    let status = run_with_input("prompt > /dev/full\n");
    assert_eq!(status.status.code(), Some(1));

    // The same holds for a *diagnostic* the builtin writes: `2>` applies to the
    // shell's own fd 2, so a misuse whose message cannot be delivered must still
    // return that misuse's status rather than abort the shell.
    let diagnostic = run_with_input("pwd extra 2> /dev/full\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&diagnostic.stdout),
        "after\n",
        "{:?}",
        diagnostic.stdout
    );
    assert!(
        !String::from_utf8_lossy(&diagnostic.stderr).contains("panicked"),
        "{:?}",
        diagnostic.stderr
    );
    let misuse = run_with_input("pwd extra 2> /dev/full\n");
    assert_eq!(misuse.status.code(), Some(1));
}

#[test]
fn a_pipeline_stage_expands_its_words_before_its_redirect_target() {
    // A stage reports the same first failure the unpiped command does, so the
    // diagnostic does not change just because the command joined a pipeline.
    let plain = run_with_input("puts $arg_missing > $redir_missing\n");
    let piped = run_with_input("puts $arg_missing > $redir_missing | cat\n");
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains("arg_missing: unbound variable"),
        "{:?}",
        plain.stderr
    );
    assert_eq!(
        String::from_utf8_lossy(&piped.stderr),
        String::from_utf8_lossy(&plain.stderr)
    );

    // The same order keeps a stage's glob from seeing the file its own `>` is
    // about to create.
    let dir = fresh_dir("stage_expand_order");
    let out = run_with_input(&format!(
        "cd {}\nputs one > a\nputs * > listing | cat\ncat listing\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a\n",
        "{:?}",
        out.stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_in_shell_command_runs_in_the_background() {
    // Backgrounding needs a child, so a builtin or a function is forked too.
    let dir = fresh_dir("background_in_shell");
    let out = run_with_input(&format!(
        "cd {}\nfunc report() {{ puts done > out }}\nreport &\nputs said > from-builtin &\n\
         sleep 0.2\ncat out from-builtin\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\nsaid\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reaped_job_notice_stays_with_the_shell() {
    // Only the shell can `waitpid`, so it refreshes the table before forking a
    // stage that can look at it. The `[N] Done` notice it produces is the
    // *shell's*, not that stage's output — bash writes it to the shell's stderr
    // whether the command is piped or not, and only the shell knows the reap
    // happened. Handing it to a stage would mean guessing which stage runs `jobs`,
    // which is unknowable for a function body.
    let dir = fresh_dir("reap_notice_owner");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    let done = |out: &Output| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|line| line.contains("Done"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    for (label, pipeline) in [
        ("direct `jobs`", "jobs 2> a | cat"),
        ("nested in a function", "shows 2> a | cat"),
        // Two function stages: only the second calls `jobs`, so no static guess
        // could pick the right one.
        ("two functions", "noop 2> a | shows 2> b"),
    ] {
        for name in ["a", "b"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let out = run_with_input(&format!(
            "cd {}\nfunc noop() {{ true }}\nfunc shows() {{ jobs }}\n             sleep 0.05 &\nsleep 0.3\n{pipeline}\n",
            dir.display()
        ));
        assert_eq!(
            done(&out),
            ["[1] Done (0) sleep 0.05"],
            "{label}: the notice belongs on the shell's stderr"
        );
        assert_eq!(read("a"), "", "{label}: a stage must not capture it");
        assert_eq!(read("b"), "", "{label}: a stage must not capture it");
    }

    // It survives a stage that never starts, for the same reason: the shell is the
    // one reporting, so nothing depends on the stage running.
    let failed =
        run_with_input("sleep 0.05 &\nsleep 0.3\njobs 2> /missing/log | cat\nputs after\n");
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("/missing/log"), "{stderr:?}");
    assert!(stderr.contains("] Done (0) sleep 0.05"), "{stderr:?}");
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "after\n");

    // And a backgrounded stage, whose targets are opened in the child.
    let backgrounded =
        run_with_input("sleep 0.05 &\nsleep 0.3\njobs 2> /missing/log &\nsleep 0.2\n");
    assert!(
        String::from_utf8_lossy(&backgrounded.stderr).contains("] Done (0) sleep 0.05"),
        "{:?}",
        backgrounded.stderr
    );

    // Reaping removes finished jobs, so a stage that cannot look at the table must
    // not trigger it: `puts hi | cat` would otherwise take a completed job out
    // from under a later `fg`, which the unpiped `puts hi` leaves alone.
    let unrelated = run_with_input("sleep 0.05 &\nsleep 0.3\nputs hi | cat\nfg\nputs end\n");
    assert!(
        !String::from_utf8_lossy(&unrelated.stderr).contains("no current job"),
        "an unrelated stage must not reap: {:?}",
        unrelated.stderr
    );
    assert_eq!(String::from_utf8_lossy(&unrelated.stdout), "hi\nend\n");

    // A nested `jobs` still reads a fresh table, which is what the refresh is for.
    let nested = |tail: &str| {
        run_with_input(&format!(
            "sleep 0.05 &\nsleep 0.3\nfunc f() {{ jobs }}\nf{tail}\n"
        ))
    };
    assert_eq!(done(&nested("")), done(&nested(" | cat")));
    assert_eq!(String::from_utf8_lossy(&nested(" | cat").stdout), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn piped_stderr_follows_the_final_stdout_for_every_stage_type() {
    // `|&` is `2>&1` appended *after* the command's own redirections, so it copies
    // wherever stdout finally points: `> f |&` takes stderr to the file and leaves
    // the next stage empty, and `2> f |&` loses the file to the pipe. An external
    // stage and an in-shell one must answer identically — the same rule bash
    // applies to both.
    let dir = fresh_dir("piped_stderr_order");
    let script = dir.join("both.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'to-stdout\\n'\nprintf 'to-stderr\\n' >&2\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    // (redirection, what the file gets, what the next stage gets)
    for (redirect, expected_file, expected_pipe) in [
        (
            "> f",
            "to-stdout
to-stderr
",
            "",
        ),
        (
            "2> f",
            "",
            "to-stdout
to-stderr
",
        ),
    ] {
        // The external spelling and the function spelling of the same command.
        for stage in ["./both.sh", "callee"] {
            let _ = std::fs::remove_file(dir.join("f"));
            let out = run_with_input(&format!(
                "cd {}\nfunc callee() {{ ./both.sh }}\n{stage} {redirect} |& cat\n",
                dir.display()
            ));
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                expected_pipe,
                "{stage} {redirect}: next stage"
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("f")).unwrap_or_default(),
                expected_file,
                "{stage} {redirect}: file"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirected_stage_still_reports_its_own_sigpipe() {
    // A SIGPIPE from a *piped* producer is the ignorable "the reader went away"
    // case. Once `> out` moves fd 1 off the pipe it is a real failure again, and
    // that must hold for a backgrounded stage too — whose redirections are opened
    // in the child, so the shell has to reason about where fd 1 *ends up* rather
    // than what it handed over.
    let dir = fresh_dir("stage_sigpipe_redirect");
    let script = |stage: &str, tail: &str| {
        format!(
            "cd {}\nfunc dies() {{ sh -c 'kill -PIPE $$' }}\n{stage} > out | cat{tail}\n",
            dir.display()
        )
    };
    // Both stage kinds defer their opens when backgrounded — the in-shell one to
    // its fork, the external one to the re-executed helper — so both have to read
    // fd 1's fate from the redirections rather than from an opened file.
    for (label, stage) in [("function", "dies"), ("external", "sh -c 'kill -PIPE $$'")] {
        // Foreground: the shell's exit status is the pipeline's.
        let foreground = run_with_input(&script(stage, ""));
        assert_eq!(
            foreground.status.code(),
            Some(141),
            "{label}: {:?}",
            foreground.stderr
        );

        // Backgrounded: the job notice carries the same status.
        let backgrounded = run_with_input(&format!("{}sleep 0.4\njobs\n", script(stage, " &")));
        let notices = String::from_utf8_lossy(&backgrounded.stderr);
        assert!(
            notices.contains("Done (141)"),
            "{label}: backgrounding hid the failure: {notices:?}"
        );

        // What matters is the *descriptor*, not the open mode: `1< file` replaces
        // fd 1 as surely as `> file` does, so it takes the stage off the pipe too.
        let by_fd = run_with_input(&format!(
            "func dies() {{ sh -c 'kill -PIPE $$' }}\n{stage} 1< /dev/null | cat &\nsleep 0.4\njobs\n"
        ));
        assert!(
            String::from_utf8_lossy(&by_fd.stderr).contains("Done (141)"),
            "{label}: `1<` should leave the pipe too: {:?}",
            by_fd.stderr
        );
    }

    // Without the redirection the SIGPIPE stays ignorable, in both spellings.
    let piped = run_with_input("func dies() { sh -c 'kill -PIPE $$' }\ndies | cat\n");
    assert_eq!(piped.status.code(), Some(0), "{:?}", piped.stderr);
    let piped_bg =
        run_with_input("func dies() { sh -c 'kill -PIPE $$' }\ndies | cat &\nsleep 0.4\njobs\n");
    assert!(
        String::from_utf8_lossy(&piped_bg.stderr).contains("Done (0)"),
        "{:?}",
        piped_bg.stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backgrounding_a_stage_does_not_move_its_piped_stderr() {
    // `|&` is `2>&1` appended after the command's own redirections, so a `> out`
    // takes stdout *and* the copy `|&` makes of it — the next stage receives
    // nothing. Adding `&` must not change that: a background stage opens its
    // targets in the child, and if the copy were made before them it would leave
    // stderr on the pipe and silently reroute the data.
    let dir = fresh_dir("background_pipe_stderr");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    // Both stage kinds: an in-shell function, which opens its targets in its own
    // fork, and an external, which defers them to the re-executed helper. The
    // merge has to happen after the targets in each.
    for (label, stage) in [
        ("function", "both"),
        ("external", "sh -c 'echo out; echo err >&2'"),
    ] {
        for name in ["fg-out", "fg-pipe", "bg-out", "bg-pipe"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let out = run_with_input(&format!(
            "cd {}\nfunc both() {{ sh -c 'echo out; echo err >&2' }}\n             {stage} > fg-out |& cat > fg-pipe\n{stage} > bg-out |& cat > bg-pipe &\nsleep 0.4\n",
            dir.display()
        ));
        assert_eq!(read("fg-out"), "out\nerr\n", "{label}: {:?}", out.stderr);
        assert_eq!(
            read("bg-out"),
            read("fg-out"),
            "{label}: backgrounding moved a stream"
        );
        assert_eq!(read("fg-pipe"), "", "{label}");
        assert_eq!(read("bg-pipe"), read("fg-pipe"), "{label}");
    }

    // The mirror case: `2>` loses to the `|&` copy, so the next stage gets both
    // streams and the file stays empty — again for either stage kind, foreground
    // or backgrounded.
    for (label, stage) in [
        ("function", "both"),
        ("external", "sh -c 'echo out; echo err >&2'"),
    ] {
        for name in ["fg-err", "fg-pipe", "bg-err", "bg-pipe"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        run_with_input(&format!(
            "cd {}\nfunc both() {{ sh -c 'echo out; echo err >&2' }}\n             {stage} 2> fg-err |& cat > fg-pipe\n{stage} 2> bg-err |& cat > bg-pipe &\nsleep 0.4\n",
            dir.display()
        ));
        assert_eq!(read("fg-err"), "", "{label}: `2>` should lose to `|&`");
        assert_eq!(read("bg-err"), read("fg-err"), "{label}");
        assert_eq!(read("fg-pipe"), "out\nerr\n", "{label}");
        assert_eq!(read("bg-pipe"), read("fg-pipe"), "{label}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_function_stage_keeps_its_typed_arguments() {
    // A function takes typed values wherever it is called, so a bare list is one
    // list-valued positional in a pipeline stage and in the background too — not
    // the "list value needs `...`" the external argv rule would give.
    let dir = fresh_dir("stage_typed_args");
    let out = run_with_input(&format!(
        "cd {}\nfunc show(xs) {{ puts \"len=$xs:len first=$xs[0]\" }}\nys = [a b]\n\
         show $ys | tr a-z A-Z\nshow $ys > out\nsleep 0.1\ncat out\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "LEN=2 FIRST=A\nlen=2 first=a\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_shell_dispatched_builtin_works_as_a_pipeline_stage() {
    // `jobs`, `fg`, `bg`, and the prompt builtins are dispatched by the shell
    // rather than by `builtins::dispatch`, so a stage that only consulted the
    // latter fell through to an external lookup and reported "command not found".
    let out = run_with_input("jobs | cat\nprompt | cat\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "mesh$ \nafter\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn job_control_stays_with_the_shell_that_owns_the_jobs() {
    // A forked stage runs the shell's code but is not the parent of its jobs, so
    // `waitpid` there fails with ECHILD. `jobs` must list the table it inherited
    // without reaping — a running job is Running, not falsely Done — and `fg`,
    // which has to wait and hand over the terminal, refuses outright.
    let listed = run_with_input("sleep 2 &\njobs | cat\nputs after\n");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("Running sleep 2"), "{stdout}");
    assert!(
        !String::from_utf8_lossy(&listed.stderr).contains("Done"),
        "{:?}",
        listed.stderr
    );

    // A job that finished since the last reap is reported the same either way:
    // the shell refreshes the table before forking the stage, since the fork
    // could not.
    let finished = run_with_input("sleep 0.05 &\nsleep 0.2\njobs | cat\n");
    let piped = String::from_utf8_lossy(&finished.stderr);
    assert!(piped.contains("Done"), "{piped}");
    assert!(
        !String::from_utf8_lossy(&finished.stdout).contains("Running"),
        "{:?}",
        finished.stdout
    );

    // Nor may a fork do the reaping on the shell's behalf: a function that pipes
    // `jobs` from a stage of its own is not the parent either, so an unguarded
    // pre-reap there would report every inherited job as finished.
    let nested = run_with_input("sleep 2 &\nfunc f() { jobs | cat }\nf | cat\nputs after\njobs\n");
    let inner = String::from_utf8_lossy(&nested.stdout);
    assert!(inner.contains("Running sleep 2"), "{inner}");
    assert!(
        !String::from_utf8_lossy(&nested.stderr).contains("Done"),
        "{:?}",
        nested.stderr
    );

    let resumed = run_with_input("sleep 2 &\nfg | cat\nputs after\n");
    assert!(
        String::from_utf8_lossy(&resumed.stderr).contains("no job control in a pipeline stage"),
        "{:?}",
        resumed.stderr
    );
    assert_eq!(String::from_utf8_lossy(&resumed.stdout), "after\n");
}

#[test]
fn a_builtin_stage_still_sees_the_previous_status() {
    // A bare `exit` reuses the preceding status, so the fork has to be handed the
    // status the pipeline started from rather than a fresh zero.
    let failed = run_with_input("false\nexit | cat\n");
    assert_eq!(failed.status.code(), Some(1));
    let succeeded = run_with_input("true\nexit | cat\n");
    assert_eq!(succeeded.status.code(), Some(0));
}

#[test]
fn a_redirect_with_no_target_is_a_syntax_error_that_recovers() {
    let out = run_with_input("echo hi >\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("redirection needs a target"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn an_empty_pipeline_stage_is_a_syntax_error_that_recovers() {
    let out = run_with_input("echo hi | |\nputs after\n");
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
fn descriptor_duplication_is_still_rejected() {
    // `2>&1`, `&>f`, `>&2`, `<&0` duplicate one descriptor onto another, which is
    // a different mechanism from retargeting one at a file — still deferred, and
    // rejected loudly rather than silently reinterpreted.
    for source in [
        "ls /nope 2>&1\n",
        "echo hello &>f\n",
        "echo hello&>f\n",
        "echo hi >&2\n",
        "cat <&0\n",
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("descriptor duplication"),
            "{source:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // An escaped `\&` is a literal, so `hi\&>f` is an ordinary redirect.
    let dir = fresh_dir("redir_escaped_amp");
    let esc = run_with_input(&format!("cd {}\necho hi\\&>f\ncat f\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&esc.stdout), "hi&\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stderr_can_be_redirected_to_a_file() {
    let dir = fresh_dir("redir_stderr");
    let err = dir.join("err.txt");
    let out = run_with_input(&format!(
        "ls /nonexistent-path 2> {}\nputs after\n",
        err.display()
    ));
    // stdout is untouched; the diagnostic went to the file.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let captured = std::fs::read_to_string(&err).unwrap();
    assert!(captured.contains("/nonexistent-path"), "{captured:?}");

    // `2>>` appends rather than truncating.
    run_with_input(&format!("ls /nonexistent-path 2>> {}\n", err.display()));
    let appended = std::fs::read_to_string(&err).unwrap();
    assert_eq!(appended.lines().count(), 2, "{appended:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_descriptor_takes_its_own_target() {
    let dir = fresh_dir("redir_both");
    let (out_path, err_path) = (dir.join("o.txt"), dir.join("e.txt"));
    let out = run_with_input(&format!(
        "sh -c 'echo O; echo E >&2' > {} 2> {}\n",
        out_path.display(),
        err_path.display()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&out_path).unwrap(), "O\n");
    assert_eq!(std::fs::read_to_string(&err_path).unwrap(), "E\n");

    // `1>` names stdout explicitly, the same as a bare `>`.
    let one = dir.join("one.txt");
    run_with_input(&format!("echo hi 1> {}\n", one.display()));
    assert_eq!(std::fs::read_to_string(&one).unwrap(), "hi\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_descriptor_prefix_must_abut_the_operator() {
    // Spacing decides, as in bash: `echo 2 > f` writes "2" to f rather than
    // redirecting descriptor 2.
    let dir = fresh_dir("redir_fd_spacing");
    let file = dir.join("f.txt");
    run_with_input(&format!("echo 2 > {}\n", file.display()));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "2\n");

    // A bare fd needs *only* digits abutting the operator, so an empty quote
    // (`""2>f`) is an ordinary argument plus a stdout redirect.
    let dir2 = fresh_dir("redir_empty_quote_fd");
    let eq = run_with_input(&format!("cd {}\necho \"\"2>f\ncat f\n", dir2.display()));
    assert_eq!(String::from_utf8_lossy(&eq.stdout), "2\n");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn stderr_redirection_reaches_in_shell_commands_too() {
    let dir = fresh_dir("redir_stderr_in_shell");
    let (f_err, p_err) = (dir.join("f.txt"), dir.join("p.txt"));

    // A redirected function applies it to the shell's own descriptors...
    let out = run_with_input(&format!(
        "func noisy() {{ sh -c 'echo out; echo err >&2' }}\nnoisy 2> {}\n",
        f_err.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(std::fs::read_to_string(&f_err).unwrap(), "err\n");

    // ...and a forked pipeline stage applies it in the child.
    let out = run_with_input(&format!(
        "func noisy() {{ sh -c 'echo out; echo err >&2' }}\nnoisy 2> {} | tr a-z A-Z\n",
        p_err.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "OUT\n");
    assert_eq!(std::fs::read_to_string(&p_err).unwrap(), "err\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_command_can_redirect_stderr() {
    // The background path defers its opens to a re-executed helper, so the
    // descriptor has to survive that argv hand-off as well as the direction does.
    let dir = fresh_dir("redir_stderr_background");
    let err = dir.join("bg.txt");
    let out = run_with_input(&format!(
        "ls /nonexistent-path 2> {} &\nsleep 0.3\n",
        err.display()
    ));
    assert!(String::from_utf8_lossy(&out.stderr).contains("[1]"));
    let captured = std::fs::read_to_string(&err).unwrap();
    assert!(captured.contains("/nonexistent-path"), "{captured:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_descriptor_above_two_is_rejected_with_a_specific_message() {
    let out = run_with_input("cat 3< f\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("higher descriptors"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
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
    let mut child = mesh_command()
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

#[test]
fn a_background_fifo_redirect_does_not_block_the_shell() {
    let dir = fresh_dir("fifo_background");
    let fifo = dir.join("f");
    if !Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let out = run_with_input(&format!(
        "cd {}\ncat < f &\nputs ready\necho payload > f\nsleep 0.05\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ready\npayload\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_redirect_does_not_require_sh_on_path() {
    let dir = fresh_dir("background_redirect_path");
    let output = dir.join("out");
    let mut child = mesh_command()
        .env("PATH", "/definitely-missing")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    writeln!(
        child.stdin.take().expect("stdin"),
        "/bin/echo ok > {} &\n/bin/sleep 0.05\njobs",
        output.display()
    )
    .expect("write commands");
    let result = child.wait_with_output().expect("wait for mesh");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "ok\n");
    assert!(!String::from_utf8_lossy(&result.stderr).contains("command not found"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_background_redirect_reports_mesh_status_one() {
    let dir = fresh_dir("background_redirect_failure");
    let missing = dir.join("missing/out");
    // The redirect helper is a separate process writing to the same stderr as the
    // shell's job notices, so assert on whole *lines*: a non-atomic write splices
    // the two together (`mesh: [1] 4242` + an orphaned remainder) and a plain
    // `contains` on a prefix would still pass. Repeat the run because the splice
    // needs the two writers to overlap, which only happens under contention.
    for _ in 0..5 {
        let out = run_with_input(&format!(
            "/bin/echo ok > {} &\n/bin/sleep 0.05\njobs\n",
            missing.display()
        ));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.lines().any(|line| line.contains("] Done (1) ")),
            "{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.starts_with(&format!("mesh: {}: ", missing.display()))),
            "{stderr}"
        );
        assert!(!stderr.contains("mesh-redir"), "{stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Functions (`func name(params) { body }`)
// ---------------------------------------------------------------------------

#[test]
fn defines_and_calls_a_function_with_a_positional() {
    let out = run_with_input("func greet(name) {\n  puts \"hi, $name\"\n}\ngreet world\n");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi, world\n");
}

#[test]
fn functions_generate_help_from_their_signatures_without_running() {
    let out =
        run_with_input("func greet(first, last) { puts BODY-RAN }\ngreet --help\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Usage: greet <FIRST> <LAST>\n\nArguments:\n  <FIRST>\n  <LAST>\n\nOptions:\n  --help  Print help\nafter\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn builtins_print_standard_command_line_help() {
    let out = run_with_input("cd --help\nputs --help\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Usage: cd [DIR]\n\nOptions:\n  --help  Print help\nUsage: puts [ARG ...]\n\nOptions:\n  --help  Print help\n"
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn option_terminator_passes_help_to_a_function_as_data() {
    let out = run_with_input("func show(value) { puts \"<$value>\" }\nshow -- --help\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<--help>\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_single_line_function_definition_works() {
    let out = run_with_input("func sq(x) { puts $x $x }\nsq 3\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3 3\n");
}

#[test]
fn a_function_body_is_parsed_when_defined() {
    let out = run_with_input("func bad() { value = 1 < 2 < 3 }\nbad\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("comparisons cannot be chained"));
    assert!(stderr.contains("command not found: bad"));
}

#[test]
fn a_function_takes_multiple_positionals() {
    // Comma-separated parameter lists bind left to right.
    let out = run_with_input("func pair(a, b) { puts $a $b }\npair x y\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x y\n");
}

#[test]
fn a_functions_status_is_its_last_command() {
    let out = run_with_input("func f() { true; false }\nf || puts caught\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "caught\n");
}

#[test]
fn return_sets_the_status_and_stops_the_body() {
    let out = run_with_input("func f() { puts one; return 3; puts two }\nf || puts nonzero\n");
    // `two` never prints (return stops the body); the status is 3, so `||` fires.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\nnonzero\n");
}

#[test]
fn an_empty_body_yields_status_zero() {
    let out = run_with_input("func nop() { }\nnop && puts ok\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn a_return_value_is_masked_to_eight_bits() {
    // `return 256` is status 0, matching `exit` and `DESIGN.md`.
    let out = run_with_input("func f() { return 256 }\nf && puts zero\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "zero\n");
}

#[test]
fn return_carries_a_typed_value_whose_status_is_a_view_of_it() {
    // `return <value>` now accepts any value, not only an integer; the exit status
    // is a view of it (`DESIGN.md` §"Functions"): an integer is its own status, a
    // boolean inverts (`true` → 0, `false` → 1), and any other type is success.
    let boolean = run_with_input(
        "func ok() { return true }\nfunc bad() { return false }\nok && puts t\nbad || puts f\n",
    );
    assert_eq!(String::from_utf8_lossy(&boolean.stdout), "t\nf\n");
    assert!(boolean.stderr.is_empty(), "{:?}", boolean.stderr);
    // A returned string or list is success — and no longer the old "numeric
    // argument required" error.
    let other = run_with_input(
        "func s() { return \"hi\" }\nfunc l() { return [1 2 3] }\ns && puts sok\nl && puts lok\n",
    );
    assert_eq!(String::from_utf8_lossy(&other.stdout), "sok\nlok\n");
    assert!(other.stderr.is_empty(), "{:?}", other.stderr);
}

#[test]
fn a_bare_return_uses_the_last_status() {
    // A `return` with no argument carries the last status, like `exit` with none.
    let out = run_with_input("func f() { false\n return }\nf || puts nonzero\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nonzero\n");
}

#[test]
fn a_lone_numeric_literal_is_a_value_not_a_command() {
    // "A block evaluates to its last expression — a bare value, a `[…]` literal, …"
    // (`DESIGN.md`). Every such spelling already worked *except* an unquoted
    // numeral, which fell through to command resolution: `func f() { 42 }` reported
    // "command not found: 42". It is a value now, and a real integer — not the
    // string "42".
    let out = run_with_input(
        "func answer() { 42 }\nfunc zero() { 0 }\nfunc big() { 1000 }\n\
         a = answer()\nb = zero()\nc = big()\n\
         sum = $a + 1\n\
         puts \"$a $b $c $sum\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42 0 1000 43\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // In statement position the value is discarded and its status is the usual view
    // of an integer — itself, as `41 + 1` already gave. The new spelling reaches an
    // existing rule rather than adding one.
    let status = run_with_input("42\n");
    assert_eq!(status.status.code(), Some(42));
    assert!(status.stderr.is_empty(), "{:?}", status.stderr);
    assert_eq!(
        run_with_input("41 + 1\n").status.code(),
        status.status.code(),
        "a lone literal and the operator form should agree"
    );
}

#[test]
fn only_a_lone_numeral_becomes_a_value() {
    // The rule is deliberately the narrowest one that closes the gap: the *whole*
    // statement must be the literal, the same shape a quoted literal already had.
    // Everything a numeral-led command could mean before still means it.
    let dir = fresh_dir("numeric_literal_statement");

    // With arguments it is still a command, so the diagnostic still names it.
    let args = run_with_input("42 foo\nputs after\n");
    assert!(
        String::from_utf8_lossy(&args.stderr).contains("command not found: 42"),
        "{:?}",
        args.stderr
    );
    assert_eq!(String::from_utf8_lossy(&args.stdout), "after\n");

    // With a redirection it is still a command, so `42 > file` still tries to run
    // one — statement position keeps the redirect reading of `>`.
    let redirect = run_with_input(&format!("cd {}\n42 > out\nputs after\n", dir.display()));
    assert!(
        String::from_utf8_lossy(&redirect.stderr).contains("command not found: 42"),
        "{:?}",
        redirect.stderr
    );
    assert_eq!(String::from_utf8_lossy(&redirect.stdout), "after\n");

    // Heading a pipeline it is still a command. An expression cannot *be* a
    // pipeline stage, so classifying `42 | cat` as one would leave the `|`
    // unconsumed and turn a command that runs today into a syntax error — which
    // rejects the whole script, a much bigger change than the diagnostic it
    // replaced.
    for piped in ["42 | cat", "42 |& cat"] {
        let out = run_with_input(&format!("{piped}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("command not found: 42"),
            "{piped}: {stderr:?}"
        );
        assert!(
            !stderr.contains("syntax error"),
            "{piped} must stay a pipeline: {stderr:?}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{piped}");
    }

    // The separators an expression statement *can* take are unaffected: 42 is a
    // nonzero status, so `&&` short-circuits and `||` runs its right side.
    let and = run_with_input("42 && puts yes\nputs end\n");
    assert_eq!(String::from_utf8_lossy(&and.stdout), "end\n");
    let or = run_with_input("42 || puts no\n");
    assert_eq!(String::from_utf8_lossy(&or.stdout), "no\n");

    // Two places where the classification *does* show through, both consistent with
    // rules that already existed. In condition position the literal is a value whose
    // status is itself, so a nonzero one takes `else` — the same branch as before,
    // without the spurious "command not found" on the way.
    let condition = run_with_input("if 42 { puts t } else { puts f }\n");
    assert_eq!(String::from_utf8_lossy(&condition.stdout), "f\n");
    assert!(condition.stderr.is_empty(), "{:?}", condition.stderr);

    // And `&` on an expression is refused, as it is for any non-command statement,
    // rather than backgrounding a command named `42`. Recoverable either way.
    let backgrounded = run_with_input("42 &\nputs after\n");
    assert!(
        String::from_utf8_lossy(&backgrounded.stderr).contains("backgrounding an expression"),
        "{:?}",
        backgrounded.stderr
    );
    assert_eq!(String::from_utf8_lossy(&backgrounded.stdout), "after\n");

    // A word only *spelled* like a numeral is not one. `4"2"`, `42""`, and `4\2`
    // all compose to the text `42`, but expansion keeps the quoted and escaped
    // pieces and yields the **string** — so the predicate asks for a single *bare*
    // text piece, not merely concatenated text that parses. These three were
    // already string expressions before this change (the quoted-literal arm owns
    // them); what matters is that they still are, and are not mistaken for the new
    // integer rule.
    for spelled in ["4\"2\"", "42\"\"", "4\\2"] {
        let out = run_with_input(&format!(
            "func f() {{ {spelled} }}\nv = f()\nn = $v + 1\nputs \"[$v] $n\"\n"
        ));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("expected integer"),
            "{spelled} should still be the string \"42\": {stderr:?}"
        );
    }
    // The bare spelling is the integer, and arithmetic on it works.
    let bare = run_with_input("func f() { 42 }\nv = f()\nn = $v + 1\nputs $n\n");
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "43\n");

    // A bare word that happens to name a command is untouched: `true` is the
    // command, and its status is the block's value.
    let word = run_with_input("func t() { true }\nv = t()\nputs $v\n");
    assert_eq!(String::from_utf8_lossy(&word.stdout), "0\n");

    // mesh has no float literals, so `3.5` is still just a word — and still a
    // command. Closing that would mean adding a type, not a parse rule.
    let float = run_with_input("func f() { 3.5 }\nv = f()\nputs after\n");
    assert!(
        String::from_utf8_lossy(&float.stderr).contains("command not found: 3.5"),
        "{:?}",
        float.stderr
    );

    // A *negative* literal lexes as the minus operator followed by `3`, not as one
    // numeric word, so it does not reach this rule; `return -3` and `(-3)` both
    // carry it, and both are already the documented spellings.
    let negative = run_with_input("func f() { -3 }\nv = f()\nputs after\n");
    assert!(
        String::from_utf8_lossy(&negative.stderr).contains("command not found: -3"),
        "{:?}",
        negative.stderr
    );
    let carried = run_with_input(
        "func f() { return -3 }\nfunc g() { (-3) }\n\
         a = f()\nb = g()\nputs \"$a $b\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&carried.stdout), "-3 -3\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_value_call_returns_the_functions_value() {
    // `f(args)` calls a function for its value: the last expression, or an explicit
    // `return`. Positionals are comma-separated inside the parens.
    let out = run_with_input(
        "func add(a, b) { $a + $b }\nfunc greet(who) { return \"hi $who\" }\nn = add(2, 3)\nm = greet(world)\nputs \"$n $m\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "5 hi world\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_value_call_binds_named_options_like_flags() {
    // `key: value` options bind the same parameter as `--key` (`force: true` ≡
    // `--force`), in any order, and omitted flags take their defaults.
    let src =
        "func deploy(target, --region = us-west, --force) { return \"$target/$region/$force\" }\n";
    let out = run_with_input(&format!(
        "{src}a = deploy(prod, region: eu, force: true)\nb = deploy(app)\nputs \"$a | $b\"\n"
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "prod/eu/true | app/us-west/false\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_value_call_spreads_lists_as_positionals_and_maps_as_options() {
    let out = run_with_input(
        "func d(target, --region = us, --force) { return \"$target/$region/$force\" }\nfunc sum3(a, b, c) { $a + $b + $c }\nopts = [region: eu, force: true]\nxs = [10 20 30]\nr = d(prod, ...$opts)\nt = sum3(...$xs)\nputs \"$r $t\"\n",
    );
    // The spread map fills the options; the spread list fills the positionals.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "prod/eu/true 60\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_value_call_accepts_the_dashed_option_spelling() {
    // `--flag` and `key: value` are interchangeable inside a value call
    // (`DESIGN.md` §"Calling for a value"), so both bind the same parameter — and a
    // dashed value types like the same token in command position (`--n=2` → int).
    let src = "func d(target, --force, --tag = latest) { return \"$target/$force/$tag\" }\n";
    let out = run_with_input(&format!(
        "{src}r = d(prod, --force)\ns = d(prod, --tag=v2)\nt = d(prod, force: true, tag: v9)\nputs \"$r | $s | $t\"\n"
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "prod/true/latest | prod/false/v2 | prod/true/v9\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_value_call_honors_the_option_terminator_and_scans_spread_elements() {
    // A bare `--` ends option parsing inside a value call too, so a following
    // `--force` reaches the rest parameter as data instead of setting the switch.
    let terminated = run_with_input(
        "func f(--force, ...rest) { puts \"force=$force\"\n puts ...$rest }\nx = f(--, --force)\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&terminated.stdout),
        "force=false\n--force\n"
    );
    assert!(terminated.stderr.is_empty(), "{:?}", terminated.stderr);

    // A spread explodes into call arguments, so a `--flag` element binds its
    // option rather than becoming a positional (which would be an arity error).
    let spread = run_with_input(
        "func g(target, --force) { puts \"$target/$force\" }\nargs = [--force]\ny = g(prod, ...$args)\n",
    );
    assert_eq!(String::from_utf8_lossy(&spread.stdout), "prod/true\n");
    assert!(spread.stderr.is_empty(), "{:?}", spread.stderr);

    // Structured options cannot bind after the terminator either. A `key: value`
    // has no positional meaning to fall back to (unlike a dashed word), so it is a
    // recoverable error rather than silently binding — for a direct pair and for a
    // spread map's entries alike.
    for source in [
        "func f(--force, ...rest) { puts \"force=$force\" }\nx = f(--, force: true)\nputs after\n",
        "func f(--force, ...rest) { puts \"force=$force\" }\nopts = [force: true]\nx = f(--, ...$opts)\nputs after\n",
    ] {
        let out = run_with_input(source);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("option `force:` cannot follow `--`"),
            "{source:?}: {stderr}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "after\n",
            "{source:?}"
        );
    }
}

#[test]
fn control_flow_in_a_value_call_argument_belongs_to_the_caller() {
    // An argument expression is evaluated in the caller's scope, so a `return` it
    // raises unwinds the *caller* — it must not be captured as the callee's result.
    let returned = run_with_input(
        "func id(v) { return $v }\nfunc outer() { x = id(if true { return early })\n puts \"NOT REACHED $x\" }\nr = outer()\nputs \"outer=[$r]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&returned.stdout), "outer=[early]\n");
    assert!(returned.stderr.is_empty(), "{:?}", returned.stderr);

    // Likewise a `break` belongs to the caller's loop rather than being cleared.
    let broke = run_with_input(
        "func id(v) { return $v }\nfor i in [1 2 3] {\n  x = id(if true { break })\n  puts \"iter $i\"\n}\nputs done\n",
    );
    assert_eq!(String::from_utf8_lossy(&broke.stdout), "done\n");

    // Evaluation stops at the argument that raised it, so a later argument never
    // runs (no division error) and a named option whose expression broke is never
    // bound (no spurious type error).
    let later = run_with_input(
        "func two(a, b) { return \"$a/$b\" }\nfor i in [1 2] {\n  x = two(if true { break }, 1 / 0)\n  puts \"iter $i\"\n}\nputs done\n",
    );
    assert_eq!(String::from_utf8_lossy(&later.stdout), "done\n");
    assert!(later.stderr.is_empty(), "{:?}", later.stderr);

    let named = run_with_input(
        "func f(a, --force) { return $a }\nfor i in [1 2] {\n  x = f(if true { break }, force: if true { break })\n  puts \"iter $i\"\n}\nputs done\n",
    );
    assert_eq!(String::from_utf8_lossy(&named.stdout), "done\n");
    assert!(named.stderr.is_empty(), "{:?}", named.stderr);

    // `continue` still skips only its own iteration.
    let continued = run_with_input(
        "func id(v) { return $v }\nfor i in [1 2 3] {\n  x = id(if $i == 2 { continue })\n  puts \"iter $i\"\n}\nputs done\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&continued.stdout),
        "iter 1\niter 3\ndone\n"
    );

    // The control flow may be raised from *within* a compound argument: the rest of
    // that expression must not run either, so no operator is applied to a value
    // that was never produced (which would report a spurious type error).
    for source in [
        "func two(a, b) { return \"$a/$b\" }\nfor i in [1 2] {\n  x = two((if true { break }) + 1, 2)\n  puts \"iter $i\"\n}\nputs done\n",
        "func one(a) { return $a }\nfor i in [1 2] {\n  x = one(-(if true { break }))\n  puts \"iter $i\"\n}\nputs done\n",
    ] {
        let out = run_with_input(source);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n", "{source:?}");
        assert!(out.stderr.is_empty(), "{source:?}: {:?}", out.stderr);
    }
}

#[test]
fn pending_loop_control_stops_every_expression_wrapper() {
    // `break`/`continue` travel beside the value channel, so a wrapper whose child
    // raised one has no value to work with: it must stop rather than report a
    // spurious error about a value that was never produced. This holds for plain
    // expressions, not just value-call arguments.
    for source in [
        // member access
        "for i in [1] {\n  y = (if true { break }).field\n}\nputs done\n",
        // index and slice bounds
        "xs = [1 2 3]\nfor i in [1] {\n  a = $xs[if true { break }]\n}\nputs done\n",
        "xs = [1 2 3]\nfor i in [1] {\n  b = $xs[(if true { break })..2]\n}\nputs done\n",
        // modifier
        "for i in [1] {\n  c = (if true { break }):upper\n}\nputs done\n",
        // range endpoint
        "for i in [1] {\n  d = (if true { break })..3\n}\nputs done\n",
        // and through a value call's argument
        "func id(v) { return $v }\nfor i in [1] {\n  e = id((if true { break }).foo)\n}\nputs done\n",
    ] {
        let out = run_with_input(source);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n", "{source:?}");
        assert!(out.stderr.is_empty(), "{source:?}: {:?}", out.stderr);
    }
}

#[test]
fn pending_loop_control_stops_every_statement_consumer() {
    // The wrapper audit covers expressions that *build* a value. A statement that
    // *acts* on one is the other half: a `for` iterable, an `if`/`while`
    // condition, a `match` subject, a guard. Each must see that no truth value
    // was produced rather than treat the placeholder as one — otherwise the body
    // runs, and so do the statements after it, before the loop unwinds.
    //
    // The outer `for` runs twice if the `break` is not honored, so a body that
    // leaks would print twice as well.
    let call = "func id(v) { return $v }\n";
    for (label, source) in [
        (
            "for iterable",
            "for a in [1 2] {\n  for i in id(if true { break }) { puts LEAK }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "if condition",
            "for a in [1 2] {\n  if id(if true { break }) { puts LEAK } else { puts LEAK }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "match subject",
            "for a in [1 2] {\n  match id(if true { break }) { _ { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "guard",
            "for a in [1 2] {\n  puts LEAK if id(if true { break })\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "expression statement",
            "for a in [1 2] {\n  id(if true { break })\n  puts LEAK-AFTER\n}\n",
        ),
    ] {
        let out = run_with_input(&format!("{call}{source}puts done\n"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "done\n",
            "{label}: {source:?}"
        );
        assert!(out.stderr.is_empty(), "{label}: {:?}", out.stderr);
    }

    // A `match` arm's *guard* is the same: its falsy placeholder must not read as
    // "this arm does not match", or a later arm runs while the loop is unwinding.
    for (label, source) in [
        (
            "statement mode",
            "for a in [1 2] {\n  match 7 { 7 if (if true { break }) { puts LEAK } _ { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "value mode",
            "for a in [1 2] {\n  v = match 7 { 7 if (if true { break }) { puts LEAK\n 1 } _ { puts LEAK\n 2 } }\n  puts LEAK-AFTER\n}\n",
        ),
    ] {
        let out = run_with_input(&format!("{source}puts done\n"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "done\n",
            "match guard, {label}: {source:?}"
        );
        assert!(
            out.stderr.is_empty(),
            "match guard, {label}: {:?}",
            out.stderr
        );
    }

    // A match *pattern* is an expression too, so it can raise control while being
    // compared. "No match" against a placeholder is not an answer either.
    for (label, source) in [
        (
            "statement mode",
            "for a in [1 2] {\n  match 1 { id(if true { break }) { puts LEAK } _ { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "value mode",
            "for a in [1 2] {\n  v = match 1 { id(if true { break }) { 1 } _ { puts LEAK\n 2 } }\n  puts LEAK-AFTER\n}\n",
        ),
    ] {
        let out = run_with_input(&format!("{call}{source}puts done\n"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "done\n",
            "match pattern, {label}: {source:?}"
        );
        assert!(
            out.stderr.is_empty(),
            "match pattern, {label}: {:?}",
            out.stderr
        );
    }

    // A `continue` raised by a `while` *condition* targets that loop and re-tests
    // it — the condition may have changed the state the next test reads, so
    // ending the loop instead would skip passes that should run.
    let retested = run_with_input(
        "n = 0\nwhile (if $n == 0 { n = 1\n continue } else { $n != 3 }) { puts \"pass n=$n\"\n n = $n + 1 }\nputs done\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&retested.stdout),
        "pass n=1\npass n=2\ndone\n",
        "{:?}",
        retested.stdout
    );
    assert!(retested.stderr.is_empty(), "{:?}", retested.stderr);

    // A `continue` in the same position leaves the pass without running the body,
    // and the loop still makes its remaining passes.
    let resumed = run_with_input(&format!(
        "{call}for a in [1 2] {{\n  for i in id(if $a == 1 {{ continue }} else {{ [ok] }}) {{ puts \"iter=$i\" }}\n  puts \"pass=$a\"\n}}\nputs done\n"
    ));
    assert_eq!(
        String::from_utf8_lossy(&resumed.stdout),
        "iter=ok\npass=2\ndone\n",
        "{:?}",
        resumed.stdout
    );
    assert!(resumed.stderr.is_empty(), "{:?}", resumed.stderr);
}

#[test]
fn a_statements_operands_do_not_become_its_result() {
    // A condition, a `match` subject, a `for` iterable, an assignment's
    // right-hand side, a guard: each is an *operand* of the statement around it,
    // not the statement. An operand that runs statements of its own records
    // results while doing so, and those belong to the operand — the enclosing
    // executable still reports its own.
    //
    // Every header here ends in a truthy value but records a `false` on the way,
    // so a leak shows up as `1` (that `false`'s status) instead of `7`.
    let headers = run_with_input(
        "func ifc() { 7 + 0\n if (if true { false\n 1 == 1 }) { return } }\n         func whilec() { 7 + 0\n while (if true { false\n 1 == 1 }) { return } }\n         func matchc() { 7 + 0\n match (if true { false\n 9 + 0 }) { _ { return } } }\n         func forc() { 7 + 0\n for i in (if true { false\n [1] }) { return } }\n         a = ifc()\nb = whilec()\nc = matchc()\nd = forc()\n         puts \"[$a][$b][$c][$d]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&headers.stdout),
        "[7][7][7][7]\n",
        "{:?}",
        headers.stdout
    );
    assert!(headers.stderr.is_empty(), "{:?}", headers.stderr);

    // An assignment reports its own *status*, so a compound right-hand side must
    // not leave the assignment looking like a value-producing statement.
    let assigned = run_with_input(
        "func plain() { x = if true { 5 + 0\n 6 + 0 }\n return }\n         func env() { 7 + 0\n $env.MESH_OPERAND = if true { 5 + 0\n 6 + 0 }\n return }\n         a = plain()\nb = env()\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&assigned.stdout),
        "[0][0]\n",
        "{:?}",
        assigned.stdout
    );

    // A guarded *pipeline* that is skipped produced nothing, exactly as a guarded
    // expression does, so the typed result before it still stands.
    let skipped =
        run_with_input("func f() { 1 == 2\n puts no if false\n return }\nx = f()\nputs \"[$x]\"\n");
    assert_eq!(
        String::from_utf8_lossy(&skipped.stdout),
        "[false]\n",
        "{:?}",
        skipped.stdout
    );
}

#[test]
fn a_rejected_background_list_records_its_own_failure() {
    // The rejection returns above the layer that normally records a statement's
    // result, so it has to record its own. Otherwise the value an earlier
    // statement produced still stands and a bare `return` carries that instead of
    // the failure — the call would look like it succeeded.
    let out =
        run_with_input("func f() { 5 + 0\n true && false &\n return }\nx = f()\nputs \"x=[$x]\"\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "x=[2]\n",
        "{:?}",
        out.stdout
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("background conditional lists"),
        "{:?}",
        out.stderr
    );
}

#[test]
fn an_assignment_whose_value_broke_keeps_the_previous_binding() {
    // The right-hand side produced no value, so the target must be left alone
    // rather than overwritten with a placeholder.
    let out =
        run_with_input("x = keep\nfor i in [1] {\n  x = if true { break }\n}\nputs \"x=[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x=[keep]\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // An environment assignment is a place too, whether the value came from an
    // expression or from a value call.
    let env = run_with_input(
        "func id(v) { return $v }\n$env.MESH_KEEP = keep\n\
         for i in [1] {\n  $env.MESH_KEEP = if true { break }\n}\n\
         for i in [1] {\n  $env.MESH_KEEP = id(if true { break })\n}\n\
         puts \"env=[$env.MESH_KEEP]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&env.stdout), "env=[keep]\n");
    assert!(env.stderr.is_empty(), "{:?}", env.stderr);
}

#[test]
fn a_builtin_value_constructor_cannot_be_a_function_name() {
    // `re(...)` is a built-in value constructor, so a `func re` would be reachable
    // as a command but never as a value call — reserve the name instead of
    // shipping a function whose meaning depends on how it is called. The error is
    // recoverable: the next command still runs.
    let out = run_with_input("func re(x) { return $x }\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`re` is a built-in value constructor"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // A name that merely contains or resembles it is unaffected.
    let ok = run_with_input("func read(x) { return \"ok:$x\" }\ny = read(v)\nputs \"$y\"\n");
    assert_eq!(String::from_utf8_lossy(&ok.stdout), "ok:v\n");
    assert!(ok.stderr.is_empty(), "{:?}", ok.stderr);
}

#[test]
fn a_value_call_evaluates_arguments_in_the_callers_scope() {
    // `f($x)` reads the caller's `$x`, not the callee's fresh scope.
    let out = run_with_input("func id(v) { return $v }\nx = outer\ny = id($x)\nputs \"$y\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "outer\n");
}

#[test]
fn a_bare_return_carries_the_result_so_far() {
    // `return` on its own exits early carrying the body's result *so far*, not a
    // freshly minted status: a value the body produced, the status of a command
    // that produced none, and the empty string when nothing ran at all.
    let out = run_with_input(
        "func carried() { x = hello\n $x\n return }\n\
         func empty() { return }\n\
         func failed() { false\n return }\n\
         a = carried()\nb = empty()\nputs \"[$a][$b]\"\n\
         failed || puts command-status\nfailed() || puts value-status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[hello][]\ncommand-status\nvalue-status\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // The `return` can be *inside* an `&&` / `||` list, where the result so far
    // is the executable that just ran in the same list, not the statement before.
    let chained = run_with_input(
        "func f() { false || return }\nf && puts bad\nf || puts ok\nv = f()\nputs \"v=[$v]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&chained.stdout), "ok\nv=[1]\n");
    assert!(chained.stderr.is_empty(), "{:?}", chained.stderr);

    // What produced the result is *observed*: a branch's value survives the `if`
    // that ran it, and an expression that failed leaves no stale value behind.
    let observed = run_with_input(
        "func nested() { if true { 1 == 2 }\n return }\n\
         func failed() { 2 + 3\n 1 / 0\n return }\n\
         a = nested()\nb = failed()\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&observed.stdout), "[false][1]\n");

    // A call is a command like any other: the callee's own result, and its mark,
    // stay with the callee, so the caller records the call's *status*.
    let nested = run_with_input(
        "func inner() { 7 + 0 }\nfunc outer() { 42 + 0\n inner\n return }\n\
         x = outer()\nputs \"x=[$x]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&nested.stdout), "x=[7]\n");

    // An argument's own statements record results of their own; they belong to
    // neither side, so a failing call still records its failure.
    let argument = run_with_input(
        "func need2(a, b) { return \"$a$b\" }\n\
         func outer() { need2(if true { 0 + 0\n 6 + 0 })\n return }\n\
         outer && puts bad\nouter || puts ok\n",
    );
    assert_eq!(String::from_utf8_lossy(&argument.stdout), "ok\n");

    // A guard that fails leaves the statement unrun, so the previous result — a
    // typed one — still stands; and a compound argument's own result belongs to
    // the setup, not to a body that has produced nothing yet.
    let skipped = run_with_input(
        "func f() { 1 == 2\n 4 + 5 if false\n return }\n\
         func g(x) { return }\n\
         a = f()\nb = g(if true { 1 + 1 })\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&skipped.stdout), "[false][]\n");

    // A quoted scalar statement is a *command* — the spelling that runs a path
    // with spaces — so its result is its status, not the string.
    let quoted = run_with_input(
        "func f() { \"false\"\n return }\nf && puts bad\nf || puts ok\nv = f()\nputs \"v=[$v]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "ok\nv=[1]\n");

    // A compound that ran but produced no value results in the empty string — not
    // the result the statement before it recorded, and not its own status. That is
    // what the same construct yields in value position, so the two agree.
    let empty_compound = run_with_input(
        "func branch() { 5 + 0\n if true { }\n return }\n         func unbranched() { 5 + 0\n if false { 1 + 1 }\n return }\n         func elsewhere() { 5 + 0\n if false { 9 } else { }\n return }\n         func unmatched() { 5 + 0\n match 1 { 2 { 3 + 3 } }\n return }\n         func unlooped() { 5 + 0\n while false { 1 + 1 }\n return }\n         a = branch()\nb = unbranched()\nc = elsewhere()\nd = unmatched()\ne = unlooped()\n         puts \"[$a][$b][$c][$d][$e]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&empty_compound.stdout),
        "[][][][][]\n",
        "{:?}",
        empty_compound.stdout
    );
    assert!(
        empty_compound.stderr.is_empty(),
        "{:?}",
        empty_compound.stderr
    );
    // A branch that *did* produce keeps its value, so the normalization only
    // applies to a construct that produced nothing.
    let produced = run_with_input(
        "func f() { 5 + 0\n if true { 1 == 2 }\n return }\na = f()\nputs \"[$a]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&produced.stdout), "[false]\n");

    // A compound executable reports *its own* value, not its last nested one's: a
    // `for` collects a value per pass, so a bare `return` after one carries the
    // aggregate list — the same value the loop yields as the body's last
    // statement. A pass that produced nothing contributes the empty string.
    let aggregate = run_with_input(
        "func carried() { for i in [1 2] { $i + 10 }\n return }\n         func direct() { for i in [1 2] { $i + 10 } }\n         func silent() { for i in [1 2] { }\n return }\n         a = carried()\nb = direct()\nc = silent()\n         puts \"[$a[0]][$a[1]][$b[0]][$b[1]][$c[0]][$c[1]]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&aggregate.stdout),
        "[11][12][11][12][][]\n",
        "{:?}",
        aggregate.stdout
    );
    assert!(aggregate.stderr.is_empty(), "{:?}", aggregate.stderr);

    // An argument is still the *caller's* code, so a bare `return` raised while
    // evaluating one carries the caller's result so far — the same value the same
    // `return` carries outside the call. A default is the callee's code, so its
    // bare `return` carries the callee's result, which is nothing yet.
    let in_argument = run_with_input(
        "func id(v) { v }\n\
         func called() { 5 + 0\n x = id(if true { return })\n puts bad }\n\
         func plain() { 5 + 0\n if true { return }\n puts bad }\n\
         func defaulted(x = if true { return }) { puts bad }\n\
         a = called()\nb = plain()\nc = defaulted()\nputs \"[$a][$b][$c]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&in_argument.stdout), "[5][5][]\n");
    assert!(in_argument.stderr.is_empty(), "{:?}", in_argument.stderr);
}

#[test]
fn a_result_reports_the_status_view_of_its_value() {
    // An expression statement's status is the view of its value (`DESIGN.md`): an
    // integer is its own status, a boolean inverts, anything else is 0. That
    // holds for a value call, for a command-mode call whose body ends in an
    // implicit value, and for a bare expression.
    let out = run_with_input(
        "func f() { return false }\nfunc g() { 1 == 2 }\nfunc t() { 1 == 1 }\nfunc n() { return 3 }\n\
         f() && puts bad-value-call\nf() || puts ok-value-call\n\
         g && puts bad-command-call\ng || puts ok-command-call\n\
         t && puts ok-true\n\
         n() || puts ok-integer\n\
         1 == 2 || puts ok-bare\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok-value-call\nok-command-call\nok-true\nok-integer\nok-bare\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn an_integer_result_becomes_the_exit_status() {
    // The integer view reaches the shell's own exit status, not just `&&` / `||`.
    let out = run_with_input("func n() { return 3 }\nn()\n");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn a_conditional_list_is_still_the_functions_value() {
    // A final `&&` / `||` list is not a tail expression, but the branch that ran
    // still produced the body's result.
    let out = run_with_input(
        "func f() { 1 == 2 || 3 + 4 }\nfunc g() { 1 == 1 && 4 + 5 }\nx = f()\ny = g()\nputs \"[$x][$y]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[7][9]\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_body_that_produced_nothing_yields_the_empty_string() {
    // An empty body does not inherit whatever the surrounding code last
    // recorded: each iteration here produced nothing, so each element is empty.
    let out = run_with_input(
        "9 + 0\nxs = for i in [1 2] { }\na = $xs[0]\nb = $xs[1]\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[][]\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_guarded_final_expression_is_still_the_functions_value() {
    // A guard on the last statement does not stop it being the body's value; a
    // guard that fails leaves the expression unevaluated, so there is no value.
    let out = run_with_input(
        "func f() { 1 + 1 if true }\nfunc g() { 1 + 1 if false }\nx = f()\ny = g()\nputs \"[$x][$y]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[2][]\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // A guard only decides *whether* the statement runs; it is not the statement.
    // A condition that runs commands of its own must not leave its last status as
    // the result so far, or the `return` it guards carries the guard's bookkeeping
    // instead of what the body produced.
    let compound_guard = run_with_input(
        "func returned() { 1 + 6\n return if (if 1 == 1 { false\n 1 == 1 }) }\n         func valued() { 1 + 6\n 2 + 3 if (if 1 == 1 { false\n 1 == 1 }) }\n         a = returned()\nb = valued()\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&compound_guard.stdout),
        "[7][5]\n",
        "{:?}",
        compound_guard.stdout
    );
    assert!(
        compound_guard.stderr.is_empty(),
        "{:?}",
        compound_guard.stderr
    );

    // A guard that fails leaves the expression unrun, which produces *nothing* —
    // so an earlier statement's result still stands, exactly as a bare `return` in
    // its place would carry it. Only a body with nothing before it is empty.
    let earlier = run_with_input(
        "func kept() { 1 + 1\n 3 + 4 if false }\n         func returned() { 1 + 1\n 3 + 4 if false\n return }\n         a = kept()\nb = returned()\nputs \"[$a][$b]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&earlier.stdout),
        "[2][2]\n",
        "{:?}",
        earlier.stdout
    );
    assert!(earlier.stderr.is_empty(), "{:?}", earlier.stderr);
}

#[test]
fn an_out_of_loop_break_in_an_argument_recovers() {
    // `break` outside any loop is a runtime error, not an unwind: the statement
    // fails and the script carries on. Inside a loop the same argument leaves the
    // loop, as it should.
    let recovered =
        run_with_input("func id(v) { return $v }\nx = id(if true { break })\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&recovered.stdout), "after\n");
    assert!(
        String::from_utf8_lossy(&recovered.stderr).contains("break: not inside a loop"),
        "{:?}",
        recovered.stderr
    );

    let looped = run_with_input(
        "func id(v) { return $v }\nfor i in [1 2 3] { x = id(if true { break })\n puts \"iter $i\" }\nputs done\n",
    );
    assert_eq!(String::from_utf8_lossy(&looped.stdout), "done\n");
    assert!(looped.stderr.is_empty(), "{:?}", looped.stderr);
}

#[test]
fn a_value_call_that_broke_outside_a_loop_fails_like_the_command_call() {
    // `break` outside a loop is a runtime error. The value call must report the
    // same failure as `f` in command position, not quietly yield an empty value.
    let src = "func h() { break }\n";
    let value = run_with_input(&format!(
        "{src}z = h() && puts assigned\nz = h() || puts or-ran\n"
    ));
    assert_eq!(String::from_utf8_lossy(&value.stdout), "or-ran\n");
    let command = run_with_input(&format!("{src}h && puts assigned\nh || puts or-ran\n"));
    assert_eq!(
        String::from_utf8_lossy(&value.stdout),
        String::from_utf8_lossy(&command.stdout)
    );
    assert!(
        String::from_utf8_lossy(&value.stderr).contains("break: not inside a loop"),
        "{:?}",
        value.stderr
    );
}

#[test]
fn a_backgrounded_non_command_is_refused() {
    // `&` needs a child to run in, and only a command or pipeline has one. An
    // expression's value is produced in this shell, so there is nowhere for a
    // backgrounded value call's result to go. Refusing is the honest answer;
    // running it synchronously is not, because the statements after it would see
    // side effects `&` promised to defer.
    let out = run_with_input("func f() { puts inside }\nf() &\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after\n",
        "{:?}",
        out.stdout
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("&: backgrounding an expression is not supported yet"),
        "{:?}",
        out.stderr
    );
    // The refusal is a usage error, and it lands before the guard runs.
    let refused = run_with_input(
        "func f() { puts inside }\nfunc guard() { puts guard-ran\n return true }\nf() if guard() &\n",
    );
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty(), "{:?}", refused.stdout);

    // A command *is* backgroundable, including the lone quoted spelling that runs
    // a path needing quotes — that stays a job, not an error.
    let command = run_with_input("\"/bin/true\" &\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&command.stdout),
        "after\n",
        "{:?}",
        command.stdout
    );
    assert!(
        String::from_utf8_lossy(&command.stderr).starts_with("[1] "),
        "{:?}",
        command.stderr
    );

    // A value call reached through a *compound* statement is the same case: the
    // `if` runs in this shell, so `&` cannot defer it either, and it must not run
    // synchronously ahead of the statement after it.
    let compound =
        run_with_input("func ready() { return true }\nif ready() { puts inside } &\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&compound.stdout),
        "after\n",
        "{:?}",
        compound.stdout
    );
    assert!(
        String::from_utf8_lossy(&compound.stderr)
            .contains("&: backgrounding an `if` is not supported yet"),
        "{:?}",
        compound.stderr
    );

    // Every construct that runs in the shell names itself in the diagnostic.
    for (source, noun) in [
        ("for i in [1] { puts x } &", "a `for` loop"),
        ("while false { puts x } &", "a `while` loop"),
        ("x = 1 + 1 &", "an assignment"),
        ("$env.KEY = one &", "an environment assignment"),
        ("func later() { puts x } &", "a function definition"),
    ] {
        let out = run_with_input(&format!("{source}\nputs after\n"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "after\n",
            "{source}: {:?}",
            out.stdout
        );
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains(&format!("&: backgrounding {noun} is not supported yet")),
            "{source}: {:?}",
            out.stderr
        );
    }
}

#[test]
fn a_value_call_streams_stdout_independently_of_its_value() {
    // The value call reads the return value; whatever the function prints still
    // streams (the channels are independent, `DESIGN.md`).
    let out = run_with_input(
        "func work() { puts progress\n return done }\nr = work()\nputs \"got $r\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "progress\ngot done\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_lambda_is_a_value_called_through_its_variable() {
    // `func(params) { … }` with the name left off is an anonymous function *value*:
    // bind it, then value-call it through the variable. The whole signature grammar
    // comes along — defaults, `key:` options, `...rest` — since it is the same
    // parser and the same binding as a named `func`.
    let out = run_with_input(
        "double = func(x) { $x * 2 }\n\
         greet = func(who = world) { \"hello $who\" }\n\
         twice = func(x, --loud) { if $loud { return \"$x!\" }\n return $x }\n\
         count = func(...xs) { $xs:len }\n\
         a = $double(5)\n\
         b = $greet()\n\
         c = $greet(mesh)\n\
         d = $twice(hi, loud: true)\n\
         e = $count(1, 2, 3)\n\
         puts \"$a | $b | $c | $d | $e\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "10 | hello world | hello mesh | hi! | 3\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn the_higher_order_modifiers_apply_a_callable_per_element() {
    // `:map` / `:filter` / `:each` are what lambdas are for (`DESIGN.md`). Each takes
    // one callable and applies it to every element; they chain with the ordinary
    // modifiers, and the callable can arrive through a variable just as well as
    // written inline.
    let out = run_with_input(
        "xs = [1 2 3 4]\n\
         doubled = $xs:map(func(x) { $x * 2 })\n\
         evens = $xs:filter(func(x) { $x % 2 == 0 })\n\
         fs = [\"a.txt\" \"b.md\" \"c.txt\"]\n\
         stems = $fs:filter(func(f) { $f:ext == txt }):map(func(f) { $f:stem })\n\
         twice = func(n) { $n * 2 }\n\
         through = [5]:map($twice)\n\
         puts ...$doubled\n\
         puts ...$evens\n\
         puts ...$stems\n\
         puts ...$through\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2 4 6 8\n2 4\na c\n10\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // `:each` runs for effect, in order, and yields mesh's "nothing" rather than the
    // list — so a chain cannot read side-effecting code as a transform.
    let each = run_with_input("r = [a b]:each(func(x) { puts got-$x })\nputs \"[$r]\"\n");
    assert_eq!(String::from_utf8_lossy(&each.stdout), "got-a\ngot-b\n[]\n");

    // An empty list is not a special case.
    let empty = run_with_input("ys = []:map(func(x) { $x })\nputs $ys:len\n");
    assert_eq!(String::from_utf8_lossy(&empty.stdout), "0\n");

    // Elements keep their types: a list element arrives as a list, not a rendering.
    let nested = run_with_input(
        "xss = [[1 2] [3]]\n\
         lens = $xss:map(func(l) { $l:len })\n\
         kept = $xss:filter(func(l) { $l:len == 2 })\n\
         puts ...$lens\n\
         puts \"$kept:len $kept[0]:len\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&nested.stdout), "2 1\n1 2\n");
}

#[test]
fn a_filter_predicate_must_answer_with_a_boolean() {
    // mesh's truthiness is the *shell's* — an integer is true when it is zero — so
    // reading a predicate loosely would make `:filter(func(x) { $x })` keep the
    // zeros, and a transform used as a predicate (`:filter(:dir)`, once a bare
    // modifier reference is callable) keep everything, since a dirname is always a
    // non-empty string. `DESIGN.md` raises that footgun as an open question and
    // leans loud; requiring `true`/`false` makes it unreachable.
    for (predicate, kind) in [
        ("func(x) { $x }", "an integer"),
        ("func(x) { \"yes\" }", "a string"),
        ("func(x) { [1] }", "a list"),
    ] {
        let out = run_with_input(&format!(
            "xs = [1 2]\nys = $xs:filter({predicate})\nputs after\n"
        ));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "modifier :filter predicate must return a boolean, got {kind}"
            )),
            "{predicate}: {stderr:?}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    }
}

#[test]
fn a_higher_order_callable_behaves_like_any_other_call() {
    // The point of routing these through the same machinery a written call uses: a
    // `return`, an arity mismatch, a runtime error, an escaped `break`, and an
    // `exit` all behave exactly as they do in `f(x)`.
    let returned = run_with_input("ys = [1 2]:map(func(x) { return $x * 10 })\nputs ...$ys\n");
    assert_eq!(String::from_utf8_lossy(&returned.stdout), "10 20\n");

    for (src, needle) in [
        // Arity is checked per element, naming the modifier that made the call.
        ("ys = [1 2]:map(func(a, b) { $a })\n", "expected 2 argument"),
        // A runtime error inside fails the statement rather than yielding a value.
        ("ys = [1]:map(func(x) { $x + $nope })\n", "unbound variable"),
        // A `break` with no loop of its own is reported and fails the call.
        ("ys = [1]:map(func(x) { break })\n", "not inside a loop"),
        // The argument has to be callable at all.
        (
            "ys = [1]:map(5)\n",
            "argument must be a function, got an integer",
        ),
        // And the subject has to be a list, with the fix pointed at.
        (
            "m = [a: 1]\nys = $m:map(func(x) { $x })\n",
            "requires a list; for a map use `:keys` or `:values` first",
        ),
        // Written bare, they report the missing argument rather than claiming to be
        // unimplemented — they are classified as argument-taking alongside
        // `:split` / `:join`.
        ("ys = [1 2]:map\n", "modifier :map requires an argument"),
        (
            "ys = [1 2]:filter\n",
            "modifier :filter requires an argument",
        ),
        ("ys = [1 2]:each\n", "modifier :each requires an argument"),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{src:?}");
    }

    // `exit` leaves the shell from inside a callable, as it does anywhere.
    let exited = run_with_input("[1 2]:each(func(x) { exit 5 })\nputs unreachable\n");
    assert_eq!(exited.status.code(), Some(5));
    assert!(exited.stdout.is_empty(), "{:?}", exited.stdout);

    // Loop state is the callee's: a `break` inside the callable does not escape into
    // the loop the caller is running.
    let looped = run_with_input(
        "for i in [1 2] {\n  ys = [9]:map(func(x) { break })\n  puts iter-$i\n}\nputs done\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&looped.stdout),
        "iter-1\niter-2\ndone\n"
    );

    // And scope is a lambda's: globals yes, the enclosing function's locals no.
    let scoped = run_with_input(
        "g = 10\nfunc outer() { n = 2\n  return [1]:map(func(x) { $x * $g })\n}\n\
         ys = outer()\nputs ...$ys\n",
    );
    assert_eq!(String::from_utf8_lossy(&scoped.stdout), "10\n");
}

#[test]
fn a_modifier_argument_that_raises_loop_control_does_not_become_the_argument() {
    // An argument is an *operand*, so it can raise `break`/`continue`. An unwinding
    // `eval_expr` hands back a placeholder empty string; type-checking that reads it
    // as the argument and reports a type error instead of leaving the loop. `:split`
    // was worse than a wrong type name — an empty separator has its own rule, so the
    // `break` surfaced as "separator must not be empty".
    for (subject, argument) in [
        // The higher-order modifiers, added here.
        ("[9]", ":map(if $i == 2 { break } else { func(x) { $x } })"),
        // And the pre-existing string-argument ones, which had the same hole.
        ("\"a b\"", ":split(if $i == 2 { break } else { \" \" })"),
    ] {
        let out = run_with_input(&format!(
            "for i in [1 2 3] {{\n  puts before-$i\n  ys = {subject}{argument}\n  puts after-$i\n}}\nputs done\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "before-1\nafter-1\nbefore-2\ndone\n",
            "{argument}: the break should leave the loop"
        );
        assert!(
            out.stderr.is_empty(),
            "{argument}: no diagnostic: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // `continue` is told apart from `break`: iteration 2 is skipped, 3 still runs.
    let continued = run_with_input(
        "for i in [1 2 3] {\n  puts before-$i\n  \
         ys = [9]:map(if $i == 2 { continue } else { func(x) { $x } })\n  \
         puts after-$i\n}\nputs done\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&continued.stdout),
        "before-1\nafter-1\nbefore-2\nbefore-3\nafter-3\ndone\n"
    );
    assert!(continued.stderr.is_empty(), "{:?}", continued.stderr);
}

#[test]
fn a_lambda_travels_as_a_value() {
    // Being a value is the point: a lambda passes to a function, which calls it
    // through the parameter it arrived in; it survives inside a list or a map and
    // is called straight out of the element; and a global binding is visible to the
    // lambda's own body, which is what makes recursion work without a name.
    let out = run_with_input(
        "func apply(f, x) { $f($x) }\n\
         double = func(n) { $n * 2 }\n\
         fact = func(n) { if $n == 0 { return 1 }\n return $n * $fact($n - 1) }\n\
         fs = [func() { return seven }]\n\
         m = [go: func() { return nine }]\n\
         a = apply($double, 5)\n\
         b = $fact(5)\n\
         c = $fs[0]()\n\
         d = $m.go()\n\
         puts \"$a | $b | $c | $d\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "10 | 120 | seven | nine\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_lambda_sees_the_globals_and_not_the_enclosing_locals() {
    // A lambda is an anonymous `func`, so it gets what a `func` gets: a fresh
    // function-local scope, its parameters, and the session globals — the two
    // levels `DESIGN.md` §"Variables and assignment" defines. It does *not* close
    // over the scope it was written in, so a lambda inside a function cannot read
    // that function's locals; the read fails loud rather than resolving to
    // something surprising.
    let sees = run_with_input("factor = 10\nf = func(x) { $x * $factor }\nv = $f(3)\nputs $v\n");
    assert_eq!(String::from_utf8_lossy(&sees.stdout), "30\n");
    assert!(sees.stderr.is_empty(), "{:?}", sees.stderr);

    let hidden = run_with_input(
        "func outer() { n = 2\n inner = func(x) { $x * $n }\n return $inner(3) }\nv = outer()\nputs done\n",
    );
    assert!(
        String::from_utf8_lossy(&hidden.stderr).contains("n: unbound variable"),
        "{:?}",
        hidden.stderr
    );
    // Loud, and recoverable: the script goes on.
    assert_eq!(String::from_utf8_lossy(&hidden.stdout), "done\n");
}

#[test]
fn a_function_value_has_no_text_form() {
    // Every other value can be bytes somewhere. A function cannot, so each place
    // that needs bytes refuses it by name rather than inventing a rendering.
    for (src, needle) in [
        // A command argument.
        (
            "f = func() { return 1 }\nputs $f\n",
            "$f: a function value has no text form",
        ),
        // An element of a spread.
        (
            "f = func() { return 1 }\nxs = [$f]\nputs ...$xs\n",
            "$xs: a function value has no text form",
        ),
        // The environment, which is bytes by definition.
        (
            "f = func() { return 1 }\n$env.MESH_TEST_FN = $f\n",
            "only strings cross into the environment, not a function",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{src:?}");
    }
}

#[test]
fn calling_a_value_that_is_not_a_function_is_a_loud_error() {
    // `$x(…)` on a non-function says so, and an unbound name reports the read
    // rather than the call. Both recover.
    for (src, needle) in [
        ("x = 5\ny = $x(1)\n", "x: value is not callable"),
        ("y = $nope(1)\n", "nope: unbound variable"),
        // A *bare* name still means the function store, not a variable: a lambda
        // needs the `$`, since a bare word is a literal string everywhere else.
        (
            "double = func(x) { $x * 2 }\ny = double(5)\n",
            "double: a command has no return value",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{src:?}");
    }
}

#[test]
fn two_lambdas_are_equal_only_when_they_are_the_same_function() {
    // Function equality is identity, as in every language with first-class
    // functions: a copied binding is the same function, a separately written one
    // with the same text is not.
    let out = run_with_input(
        "a = func() { return 1 }\nb = $a\nc = func() { return 1 }\n\
         same = $a == $b\nother = $a == $c\nputs \"$same $other\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true false\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn capture_returns_a_record_of_every_channel() {
    // `f(…):capture` runs the call and hands back all four channels at once:
    // `.value`, `.out`, `.err`, `.status`. It has to wrap *execution* — by the time
    // a value modifier saw the return value the stdout would already have streamed
    // away — so nothing the body prints reaches the terminal.
    let out = run_with_input(
        "func f() { puts to-out\nnosuchcmd\nreturn 7 }\n\
         r = f():capture\n\
         puts \"v=$r.value s=$r.status\"\n\
         puts \"out=[$r.out]\"\n\
         puts \"err=[$r.err]\"\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("v=7 s=7"), "{stdout:?}");
    // Raw, as written: no trailing-newline trim, unlike `$(…)`, so the record bakes
    // in no split policy.
    assert!(stdout.contains("out=[to-out\n]"), "{stdout:?}");
    // A diagnostic the body produced is on the err channel, which is where asking
    // for it put it — not on the shell's stderr.
    assert!(
        stdout.contains("err=[mesh: command not found: nosuchcmd\n]"),
        "{stdout:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("nosuchcmd"),
        "the captured diagnostic must not also reach the shell: {:?}",
        out.stderr
    );
    // Neither channel leaked to the terminal on the way past.
    assert!(!stdout.starts_with("to-out"), "{stdout:?}");
}

#[test]
fn capture_works_on_a_lambda_and_reads_a_captured_field() {
    // The callee is whatever a value call accepts, so a lambda captures too.
    let out = run_with_input(
        "g = func(x) { puts side\nreturn $x }\n\
         r = $g(4):capture\n\
         puts \"v=$r.value out=[$r.out]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "v=4 out=[side\n]\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn capturing_an_external_gives_every_channel_but_the_value() {
    // The one case where a value call on a command is allowed: it asks for the
    // channel record, not a return value the command has not got. A nonzero exit is
    // the answer, not a failure.
    let out = run_with_input(
        "ok = echo(hello):capture\n\
         bad = false():capture\n\
         puts \"s=$ok.status out=[$ok.out] bad=$bad.status\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "s=0 out=[hello\n] bad=1\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // `.value` does not exist for an external, so reading it is a loud
    // no-such-field — and the script recovers.
    let missing = run_with_input("r = echo(x):capture\nputs $r.value\nputs after\n");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no `value` in this map"),
        "{:?}",
        missing.stderr
    );
    assert_eq!(String::from_utf8_lossy(&missing.stdout), "after\n");
}

#[test]
fn capturing_a_builtin_runs_the_builtin() {
    // "Command" means builtin as well as external. Routing by "not a user function"
    // alone sent `puts(x):capture` to an exec that cannot find `puts` (status 127),
    // and `pwd():capture` to whatever `/bin/pwd` happens to be — a *different*
    // program answering for the builtin. Both go through the same in-shell
    // dispatcher command position uses.
    let out = run_with_input(
        "p = puts(hello):capture\n\
         w = pwd():capture\n\
         j = jobs():capture\n\
         e = echo(hi):capture\n\
         puts \"p=$p.status/[$p.out] w=$w.status j=$j.status e=$e.status/[$e.out]\"\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("p=0/[hello\n]"), "{stdout:?}");
    assert!(stdout.contains("w=0"), "{stdout:?}");
    assert!(stdout.contains("j=0"), "{stdout:?}");
    // The external path still works alongside it.
    assert!(stdout.contains("e=0/[hi\n]"), "{stdout:?}");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // `pwd` reports the real directory rather than an empty capture.
    let cwd = run_with_input("r = pwd():capture\nn = $r.out:len\nputs $n\n");
    let length: usize = String::from_utf8_lossy(&cwd.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    assert!(
        length > 1,
        "pwd should have written its path: {:?}",
        cwd.stdout
    );

    // An unknown name is still an external lookup, and its failure is data.
    let missing = run_with_input("r = nosuchcmd():capture\nputs \"s=$r.status\"\n");
    assert!(
        String::from_utf8_lossy(&missing.stdout).contains("s=127"),
        "{:?}",
        missing.stdout
    );

    // `exit` is a builtin that does not report a status into a record: its step
    // unwinds out of the capture and leaves the shell, descriptors restored on the
    // way.
    let exited = run_with_input("r = exit(3):capture\nputs unreachable\n");
    assert_eq!(exited.status.code(), Some(3));
    assert!(exited.stdout.is_empty(), "{:?}", exited.stdout);
}

#[test]
fn nothing_escapes_a_capture_through_an_inherited_descriptor() {
    // The capture holds four descriptors besides the standard ones: a backup of the
    // real stdout and stderr, and each pipe's read end. Left inheritable, they are
    // simply more open descriptors in any command the capture runs — and the backup
    // of the real stdout is a way straight past it. All four are close-on-exec, so
    // only the `dup2`-installed 0/1/2 reach the child. (`dup2` clears the flag on
    // what it installs, which is why those still cross `exec`.)
    let escape = run_with_input(
        "r = sh(-c, \"echo escaped >&5\"):capture\n\
         puts \"status=$r.status\"\n",
    );
    let stdout = String::from_utf8_lossy(&escape.stdout);
    assert!(
        !stdout.contains("escaped"),
        "output reached the shell past the capture: {stdout:?}"
    );
    assert!(
        !String::from_utf8_lossy(&escape.stderr).contains("escaped"),
        "{:?}",
        escape.stderr
    );
    // The write failed inside the capture instead, so the record reports it.
    assert!(!stdout.contains("status=0"), "{stdout:?}");

    // Directly: a captured child sees the standard descriptors and nothing of the
    // capture's own. (Linux-only listing, so it is a bonus assertion rather than
    // the one the test rests on; fd 3 is `ls`'s own handle on the directory.)
    if std::path::Path::new("/proc/self/fd").exists() {
        let listed =
            run_with_input("r = sh(-c, \"ls /proc/self/fd\"):capture\nputs \"fds=[$r.out]\"\n");
        let seen = String::from_utf8_lossy(&listed.stdout);
        for leaked in ["4", "5", "6", "7"] {
            assert!(
                !seen.contains(&format!("\n{leaked}\n")),
                "fd {leaked} leaked into the child: {seen:?}"
            );
        }
    }

    // `$(…)` has the same shape and had the same hole: it is the function `Diverted`
    // was generalized from, so it now goes through it and inherits both guarantees.
    // Fixing one of two parallel paths is the mistake that produced several of the
    // findings on this branch already.
    let substitution = run_with_input("x = $(sh -c \"echo escaped >&5\")\nputs \"after\"\n");
    assert!(
        !String::from_utf8_lossy(&substitution.stdout).contains("escaped"),
        "output escaped `$(…)`: {:?}",
        substitution.stdout
    );
    if std::path::Path::new("/proc/self/fd").exists() {
        let listed = run_with_input("y = $(sh -c \"ls /proc/self/fd\")\nputs \"fds=[$y]\"\n");
        let seen = String::from_utf8_lossy(&listed.stdout);
        for leaked in ["4", "5"] {
            assert!(
                !seen.contains(&format!("\n{leaked}")),
                "fd {leaked} leaked into a `$(…)` child: {seen:?}"
            );
        }
    }
    // Still captures what it should.
    let plain = run_with_input("z = $(echo hi)\nputs \"[$z]\"\n");
    assert_eq!(String::from_utf8_lossy(&plain.stdout), "[hi]\n");

    // And an ordinary capture is unaffected by the flag.
    let normal =
        run_with_input("r = echo(hi):capture\np = puts(x):capture\nputs \"[$r.out][$p.out]\"\n");
    assert_eq!(String::from_utf8_lossy(&normal.stdout), "[hi\n][x\n]\n");
}

#[test]
fn capture_rejects_what_it_cannot_bind_or_wrap() {
    for (src, needle) in [
        // An external has no signature, so a `key:` option has nothing to bind to.
        (
            "r = echo(x, color: never):capture\n",
            "needs a signature to bind to",
        ),
        // Nor a map spread, for the same reason.
        (
            "opts = [color: never]\nr = echo(x, ...$opts):capture\n",
            "only a list can be spread",
        ),
        // A list positional still needs `...`, as it does for any external.
        (
            "xs = [a b]\nr = echo($xs):capture\n",
            "a list needs `...` to become command arguments",
        ),
        // `:capture` wraps a *call*; on anything else it says so and points at the
        // spelling that does capture output.
        ("x = 5\ny = $x:capture\n", ":capture applies to a call"),
        // It takes no arguments of its own.
        (
            "func f() { return 1 }\nr = f():capture(2)\n",
            "does not take arguments",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{src:?}");
    }
}

#[test]
fn a_call_that_fails_outright_under_capture_reports_once_and_yields_nothing() {
    // Two different failures, told apart on purpose. A statement *inside* the body
    // failing is ordinary: the call still produces a record and its diagnostic is
    // on `.err`, tested above. The *call* failing — a bad argument count, so the
    // body never ran — fails the enclosing statement as an uncaptured value call
    // would, and its diagnostic is re-reported on the real stderr rather than
    // disappearing into a record nobody will ever read.
    let out = run_with_input("func f(a) { return $a }\nr = f():capture\nputs \"r=$r\"\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("expected 1 argument"), "{stderr:?}");
    assert_eq!(
        stderr.matches("expected 1 argument").count(),
        1,
        "reported exactly once: {stderr:?}"
    );
    // No record was bound, so the read fails too — the assignment did not happen.
    assert!(stderr.contains("r: unbound variable"), "{stderr:?}");
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
}

#[test]
fn capture_covers_the_whole_invocation_including_its_arguments() {
    // `:capture` is an *invocation-level* modifier, so everything written while
    // evaluating the call belongs in the record — including an argument that
    // prints. The external path used to build its argv before diverting, so a
    // side-effecting argument went to the terminal and the record held only the
    // command's own output. A captured mesh call never had that gap; the two agree
    // now.
    let external = run_with_input(
        "func side() { puts from-arg\nreturn x }\n\
         r = echo(side()):capture\n\
         puts \"out=[$r.out]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&external.stdout),
        "out=[from-arg\nx\n]\n"
    );
    assert!(external.stderr.is_empty(), "{:?}", external.stderr);

    let mesh = run_with_input(
        "func side() { puts from-arg\nreturn x }\nfunc takes(v) { puts $v }\n\
         r = takes(side()):capture\n\
         puts \"out=[$r.out]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&mesh.stdout),
        "out=[from-arg\nx\n]\n"
    );
}

#[test]
fn a_failed_external_capture_argument_reports_and_restores() {
    // An argument that fails does so while the descriptors are diverted, so its
    // diagnostic would vanish with the record — it is re-reported, once, on the
    // real stderr. And stdout is back, which the following command proves: a
    // descriptor left on a reader-less pipe would lose it or fail with EPIPE.
    for (src, needle) in [
        (
            "xs = [a b]\nr = echo($xs):capture\n",
            "a list needs `...` to become command arguments",
        ),
        (
            "opts = [k: v]\nr = echo(...$opts):capture\n",
            "only a list can be spread",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert_eq!(stderr.matches(needle).count(), 1, "once: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{src:?}");
    }
}

#[test]
fn a_capture_waits_for_a_background_child_that_holds_the_channel() {
    // A background job the call starts inherits the capture's pipes as its own
    // stdout and stderr, so the record is not complete until it lets go. That is
    // not a leak and it is not mesh-specific: bash's command substitution does the
    // same, and so does mesh's own `$(…)`.
    //
    //   bash -c 'set -m; f(){ sleep 6 & echo hi; }; r=$(f)'            # waits 6s
    //   bash -c 'set -m; f(){ sleep 6 >/dev/null 2>&1 & echo hi; }; …' # returns now
    //
    // Redirect the child's own streams away and there is nothing holding the pipe,
    // so the capture returns as soon as the call does.
    let timed = |script: &str| {
        let start = std::time::Instant::now();
        let out = run_with_input(script);
        (out, start.elapsed())
    };

    let (freed, quick) = timed(
        "func f() { sleep 5 > /dev/null 2> /dev/null &\nreturn ok }\n\
         r = f():capture\n\
         puts \"v=$r.value\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&freed.stdout), "v=ok\n");
    assert!(
        quick < std::time::Duration::from_secs(3),
        "a child with its own streams should not hold the capture: {quick:?}"
    );

    // Left inheriting them, the capture waits — the same answer bash gives.
    let (held, waited) =
        timed("func f() { sleep 0.5 &\nreturn ok }\nr = f():capture\nputs \"v=$r.value\"\n");
    assert_eq!(String::from_utf8_lossy(&held.stdout), "v=ok\n");
    assert!(
        waited >= std::time::Duration::from_millis(400),
        "a child inheriting the channels holds the capture: {waited:?}"
    );
}

#[test]
fn capture_survives_more_output_than_a_pipe_buffer_holds() {
    // Both channels are drained on their own threads. Reading them in sequence
    // would deadlock the moment a body filled the 64 KiB pipe buffer on the
    // channel that was not being read yet.
    let out = run_with_input(
        "func f() { seq 1 20000\nseq 1 20000 > /dev/stderr\nreturn 0 }\n\
         r = f():capture\n\
         a = $r.out:len\n\
         b = $r.err:len\n\
         puts \"$a $b\"\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 1..9 plus 10..99 … — the exact byte count matters less than "both large".
    let counts: Vec<usize> = stdout
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    assert_eq!(counts.len(), 2, "{stdout:?}");
    assert!(
        counts[0] > 100_000 && counts[1] > 100_000,
        "both channels should be fully drained: {counts:?}"
    );
}

#[test]
fn value_call_errors_recover_and_run_the_next_command() {
    // Each bad value call is a recoverable runtime error; the following command
    // still runs.
    for (src, needle) in [
        // An external command has no return value.
        ("y = grep(foo)\n", "no return value"),
        // An unknown option name.
        (
            "func f(target, --force) { return $target }\nz = f(x, nope: 1)\n",
            "unknown option `nope:`",
        ),
        // A positional passed by name.
        (
            "func g(dest) { return $dest }\nz = g(dest: x)\n",
            "passed by position",
        ),
        // Too few arguments.
        (
            "func need2(a, b) { $a + $b }\nz = need2(1)\n",
            "expected 2 argument(s), got 1",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("after"),
            "{src:?}: after did not run"
        );
    }
}

#[test]
fn a_function_local_does_not_leak_to_the_caller() {
    // `x` bound inside the function is gone after it returns.
    let out = run_with_input("func setx() { x = inside }\nsetx\nputs \"$x\"\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unbound variable"));
}

#[test]
fn scope_is_lexical_not_dynamic() {
    // `inner` cannot see `outer`'s local `x` — it sees only its own scope and the
    // global scope (a callee never sees its caller's locals).
    let out = run_with_input(
        "func inner() { puts \"got $x\" }\nfunc outer() { x = local; inner }\nouter\n",
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("x: unbound variable"));
}

#[test]
fn a_function_reads_a_global_variable() {
    let out = run_with_input("g = shared\nfunc show() { puts $g }\nshow\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "shared\n");
}

#[test]
fn a_function_can_call_another_function() {
    let out = run_with_input("func a() { puts from-a }\nfunc b() { a; puts from-b }\nb\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from-a\nfrom-b\n");
}

#[test]
fn a_redefinition_replaces_the_earlier_body() {
    let out = run_with_input("func f() { puts one }\nfunc f() { puts two }\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "two\n");
}

#[test]
fn a_nested_multi_line_definition_is_stored_whole() {
    // The nested `func inner` spans lines; only storing its first line would run
    // the rest as loose commands. `inner` is defined for later top-level calls.
    let out =
        run_with_input("func outer() {\n  func inner() {\n    puts nested\n  }\n}\nouter\ninner\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nested\n");
}

#[test]
fn an_arity_mismatch_is_a_recoverable_error() {
    let out = run_with_input("func f(a, b) { puts $a }\nf 1\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("expected 2 argument(s), got 1"), "{stderr}");
    // The shell recovers and keeps going.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_bare_return_at_top_level_is_reported_and_recoverable() {
    let out = run_with_input("return\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("return: not inside a function"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_reserved_name_cannot_be_a_function() {
    for name in ["cd", "exit", "func", "return", "jobs"] {
        let out = run_with_input(&format!("func {name}() {{ puts x }}\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("reserved name"), "{name}: {stderr}");
    }
}

#[test]
fn an_optional_positional_defaults_when_omitted() {
    let out = run_with_input(
        "func tag(image, version = latest) { puts \"$image:$version\" }\ntag app\ntag app v9\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "app:latest\napp:v9\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_switch_is_false_unless_passed() {
    let out = run_with_input("func f(--force) { puts $force }\nf\nf --force\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\ntrue\n");
}

#[test]
fn a_valued_flag_takes_its_value_or_default() {
    let out = run_with_input("func f(--tag = latest) { puts $tag }\nf\nf --tag=v9\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "latest\nv9\n");
}

#[test]
fn flags_bind_in_any_order_and_never_consume_positionals() {
    // `--force` before the positional, `--tag=` attached, and a rest tail.
    let out = run_with_input(
        "func deploy(target, --region = us-west, --force, --tag = latest, ...hosts) {\n  \
         puts \"$target $region $force $tag\"\n  puts ...$hosts\n}\n\
         deploy prod --force web1 --tag=v9 web2\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "prod us-west true v9\nweb1 web2\n"
    );
}

#[test]
fn an_attached_flag_value_is_a_typed_scalar() {
    // `--n=2` binds the integer `2`, like a positional `2` or the default, so it
    // participates in arithmetic instead of erroring as a string.
    let out = run_with_input("func add(--n = 1) { x = $n + 1; puts $x }\nadd\nadd --n=2\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n3\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn an_interpolated_flag_value_keeps_its_string_type() {
    // Only a bare literal token is typed; a quoted or interpolated value keeps its
    // expanded string type, exactly like the same value passed positionally.
    let out = run_with_input(
        "s = \"2\"\nfunc add(--n = 1) { x = $n + 1; puts $x }\nadd --n=$s\nputs after\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // A string `"2"` is not coerced for arithmetic — same error as a positional string.
    assert!(stderr.contains("expected integer"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_glob_expanded_flag_value_keeps_its_string_type() {
    // A flag word with glob syntax that matches a single path came from filesystem
    // expansion, not a bare literal, so its attached value stays a string (like a
    // positional glob) rather than being typed from its bytes.
    let dir = fresh_dir("glob_flag_value");
    std::fs::write(dir.join("--n=2"), "").expect("create file");
    let out = run_with_input(&format!(
        "cd {}\nfunc add(--n = 1) {{ x = $n + 1; puts $x }}\nadd --n=*\nputs after\n",
        dir.display()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    // `--n=*` expands to the file `--n=2`, so `$n` is the string `"2"` and arithmetic
    // errors exactly as a positional string would — it is not typed as integer `2`.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("expected integer"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_break_in_a_default_does_not_escape_to_the_callers_loop() {
    // A function called from inside a loop whose omitted block-bearing default runs
    // `break` must report it as outside a loop, fail that call, and leave the
    // caller's loop intact — not silently break out of it.
    let out = run_with_input(
        "func f(x = if true { break }) { puts body }\nfor j in [a b] {\n  f\n  puts \"iter $j\"\n}\nputs done\n",
    );
    // Both iterations run and the loop finishes; the body never runs (binding failed).
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "iter a\niter b\ndone\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("break: not inside a loop"), "{stderr}");
}

#[test]
fn a_rest_parameter_collects_the_leftover_positionals() {
    let out = run_with_input(
        "func f(first, ...rest) { puts $first\n  puts ...$rest }\nf a b c\nf solo\n",
    );
    // `f a b c` -> first=a, rest=[b c]; `f solo` -> first=solo, rest=[] (empty line).
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb c\nsolo\n\n");
}

#[test]
fn the_last_occurrence_of_a_valued_flag_wins() {
    let out = run_with_input("func f(--tag = d) { puts $tag }\nf --tag=v1 --tag=v2\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "v2\n");
}

#[test]
fn a_flag_value_can_arrive_spread_from_a_list() {
    let out = run_with_input(
        "flags = [--tag=v9 host1]\nfunc f(--tag = d, ...rest) { puts $tag\n  puts ...$rest }\nf ...$flags\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "v9\nhost1\n");
}

#[test]
fn a_default_can_reference_an_earlier_declared_flag() {
    // Parameters bind in declaration order, so a later default sees an
    // earlier-declared flag (switch or valued), supplied or defaulted.
    let out = run_with_input("func f(--force, x = $force) { puts $x }\nf --force\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\nfalse\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn an_unknown_flag_is_a_loud_error() {
    let out = run_with_input("func f(a) { puts $a }\nf --bogus x\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag `--bogus`"), "{stderr}");
    // Recoverable: the shell keeps going.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_valued_flag_without_a_value_is_an_error() {
    let out = run_with_input("func f(--tag = d) { puts $tag }\nf --tag\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("flag `--tag` requires a value"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_closed_header_with_an_invalid_default_dispatches_and_recovers() {
    // `x = ]` is a malformed default, so the definition is reported as a syntax
    // error and the following command still runs — not swallowed into the buffer.
    let out = run_with_input("func f(x = ])\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("syntax error"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn exit_from_a_default_propagates_out_of_the_call() {
    // `exit 7` inside an omitted default is real control flow: it exits the shell
    // with status 7 rather than being flattened into a binding error.
    let out = run_with_input("func f(x = if true { exit 7 }) { puts body }\nf\nputs after\n");
    assert_eq!(out.status.code(), Some(7));
    // Neither the body nor the following command runs.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

#[test]
fn return_from_a_default_ends_the_call_with_that_status() {
    // `return 7` inside an omitted default ends the call with status 7 (like the
    // body returning), so the function reads as nonzero and the shell continues.
    let out = run_with_input(
        "func f(x = if true { return 7 }) { puts body }\nf && puts ok || puts caught\nputs after\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "caught\nafter\n");
}

#[test]
fn a_switch_given_a_value_is_an_error() {
    let out = run_with_input("func f(--force) { puts $force }\nf --force=yes\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("flag `--force` is a switch and takes no value"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn the_terminator_sends_flag_like_tokens_to_the_rest() {
    let out = run_with_input(
        "func f(--force, ...rest) { puts $force\n  puts ...$rest }\nf -- --force a\n",
    );
    // `--` ends flag parsing: `--force` and `a` become rest elements.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n--force a\n");
}

#[test]
fn too_many_positionals_without_a_rest_is_an_error() {
    let out = run_with_input("func f(a, b = 1) { puts $a $b }\nf 1 2 3\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("expected at most 2 argument(s), got 3"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_missing_required_positional_with_optionals_present_reports_a_minimum() {
    let out = run_with_input("func f(a, b = 1) { puts $a $b }\nf\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("expected at least 1 argument(s), got 0"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn generated_help_shows_flags_optionals_and_rest() {
    let out = run_with_input(
        "func deploy(target, --region = us-west, --force, ...hosts) { puts x }\ndeploy --help\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Usage: deploy <TARGET> [<HOSTS>...]\n\nArguments:\n  <TARGET>\n  [<HOSTS>...]\n\nOptions:\n  --region=<REGION>\n  --force\n  --help  Print help\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn a_new_form_signature_buffers_across_lines() {
    // A flag/optional/rest signature split across lines — including the body brace
    // on a later line — must keep buffering, not dispatch as an incomplete header.
    let delayed_brace = run_with_input("func f(--force)\n{\n  puts \"$force\"\n}\nf --force\n");
    assert_eq!(String::from_utf8_lossy(&delayed_brace.stdout), "true\n");
    assert!(
        delayed_brace.stderr.is_empty(),
        "{:?}",
        delayed_brace.stderr
    );

    let multiline = run_with_input(
        "func g(\n  first,\n  --tag = latest,\n  ...rest\n) {\n  puts \"$first $tag\"\n  puts ...$rest\n}\ng app web1 web2\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&multiline.stdout),
        "app latest\nweb1 web2\n"
    );
    assert!(multiline.stderr.is_empty(), "{:?}", multiline.stderr);
}

#[test]
fn comments_in_a_multiline_signature_are_ignored() {
    // A `#` comment in a signature runs to the newline, so a `)`/`,` inside it is
    // not structure; the definition buffers to its real `)` and defines correctly.
    let out = run_with_input(
        "func h(\n  a, # first, and note )\n  b\n) {\n  puts \"$a $b\"\n}\nh 1 2\nputs after\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1 2\nafter\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_mismatched_default_delimiter_dispatches_and_recovers() {
    // A default that closes with the wrong delimiter (`[1)`, or a stray `]` inside
    // a block default) is malformed; it is reported and the following command
    // still runs, not swallowed into the buffer.
    for input in [
        "func f(x = [1)\nputs after\n",
        "func f(x = if true { 1 ])\nputs after\n",
        // A top-level stray `]`/`}` before any signature `)` is a hard mismatch:
        // dispatch it rather than swallow the following command to EOF.
        "func f(x = ]\nputs after\n",
        "func f(x = }\nputs after\n",
    ] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{input:?}");
    }
}

#[test]
fn a_reserved_or_duplicate_final_name_dispatches_and_recovers() {
    // An unclosed signature whose final name is finalized by the line break and is
    // a duplicate or reserved (`env`) is reported at once and the following command
    // still runs, not swallowed into the buffer while awaiting the body.
    for input in [
        "func f(a, a\nputs after\n",
        "func f(env\nputs after\n",
        // A reserved or duplicate name with an unfinished default is irreparable
        // too — finishing the default can't make the name valid.
        "func f(env =\nputs after\n",
        "func f(a, a =\nputs after\n",
    ] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{input:?}");
    }
}

#[test]
fn an_impossible_parameter_ordering_dispatches_and_recovers() {
    // An unclosed signature with a finalized ordering the parser cannot accept is
    // reported at once and the following command still runs, not swallowed into the
    // buffer while awaiting the body.
    for input in [
        "func f(a = 1, b\nputs after\n",
        "func f(...xs, a\nputs after\n",
        "func f(a = 1, ...xs\nputs after\n",
        // A newline between a name and its `=` detaches the default — the parser
        // finalizes the name at the break — so the header is irreparable.
        "func f(a\n= 1\nputs after\n",
        "func f(--flag\n= 1\nputs after\n",
    ] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{input:?}");
    }
}

#[test]
fn a_prefix_detached_from_its_name_dispatches_and_recovers() {
    // A `...name`/`--name` prefix requires the name to abut it; whitespace between
    // the prefix and the name is not a parameter, so the unclosed header is
    // reported and the following command still runs.
    for input in [
        "func f(... xs\nputs after\n",
        "func f(-- force\nputs after\n",
    ] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{input:?}");
    }
}

#[test]
fn a_finalized_empty_rest_or_flag_name_dispatches_and_recovers() {
    // The parser skips no whitespace between a `...`/`--` prefix and its required
    // name, so an unclosed signature whose prefix the newline left nameless can
    // never be completed: it is reported at once and the following command runs.
    for input in ["func f(...\nputs after\n", "func f(--\nputs after\n"] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{input:?}");
    }
}

#[test]
fn a_contextual_operator_default_finds_the_signature_close() {
    // `/#tag` tokenizes as one bare word with a literal `#` (the `/` is not a
    // token boundary here), so the following `)` is the real signature close.
    // The definition must complete rather than get lost buffering a phantom
    // comment that swallows the `)` to EOF.
    let out = run_with_input("func f(x = /#tag) {\n  puts $x\n}\nf\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/#tag\nafter\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_comment_after_an_operator_in_a_default_buffers() {
    // ` + ` is an operator (a word boundary), so the following `#` is a comment;
    // the default continues on the next line and the function defines correctly.
    let out = run_with_input("func f(x = 1 + # note )\n2) {\n  puts $x\n}\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_default_expression_with_delimiters_buffers_across_lines() {
    // A default containing `)`, brackets, or a quoted `)`/comma must not be
    // mistaken for the end of the signature when the body brace is on a later line.
    let parens = run_with_input("func f(x = (1 + 2))\n{\n  puts $x\n}\nf\n");
    assert_eq!(String::from_utf8_lossy(&parens.stdout), "3\n");
    assert!(parens.stderr.is_empty(), "{:?}", parens.stderr);

    let quoted = run_with_input("func g(x = \"a\\\",b\")\n{\n  puts $x\n}\ng\n");
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "a\",b\n");
    assert!(quoted.stderr.is_empty(), "{:?}", quoted.stderr);

    // A `$( … )` command capture in a default nests like a paren, so the delayed
    // body brace still buffers and the definition uses the capture's output.
    let capture = run_with_input("func h(x = $(puts hi))\n{\n  puts $x\n}\nh\n");
    assert_eq!(String::from_utf8_lossy(&capture.stdout), "hi\n");
    assert!(capture.stderr.is_empty(), "{:?}", capture.stderr);
}

#[test]
fn a_block_bearing_default_buffers_across_lines() {
    // A default that is a multiline block-bearing expression (`if`) has braces that
    // are not the function body; the reader must buffer until the signature's `)`,
    // not dispatch when the inner block closes.
    let out = run_with_input(
        "func f(x = if true {\n  1\n} else {\n  2\n}) {\n  puts $x\n}\nf\nputs after\n",
    );
    // `f` prints the default `1`; `puts after` runs at top level (nothing leaked).
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\nafter\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_declared_help_flag_is_kept_and_not_synthesized() {
    // A function that claims `--help` observes the switch in its body instead of
    // triggering the canned help, and its generated help does not duplicate the
    // entry (`DESIGN.md` §"Command resolution and help").
    let out = run_with_input("func f(--help) { puts \"help=$help\" }\nf --help\nf\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "help=true\nhelp=false\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn invalid_signature_forms_are_parser_errors() {
    for input in [
        "func f(a b) { puts hi }\n",
        "func f(a,) { puts hi }\n",
        "func f(a, a) { puts hi }\n",
        "func f(env) { puts hi }\n",
        // A required positional cannot follow an optional one.
        "func f(a = 1, b) { puts hi }\n",
        // Nothing may follow a `...rest`, and it cannot pair with an optional.
        "func f(...xs, a) { puts hi }\n",
        "func f(a = 1, ...xs) { puts hi }\n",
    ] {
        let out = run_with_input(input);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{input:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn a_function_runs_as_a_pipeline_stage() {
    let out = run_with_input("func f() { puts one\nputs two }\nf | sort -r\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "two\none\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_function_stage_reads_the_pipe_and_sits_in_the_middle() {
    // Downstream: the function's body inherits the stage's stdin.
    let out = run_with_input("func upper() { tr a-z A-Z }\necho abc | upper\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ABC\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And in the middle of three, reading one pipe and writing the next.
    let out = run_with_input("func pass() { cat }\necho mid | pass | tr a-z A-Z\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "MID\n");
}

#[test]
fn pipeline_stages_run_concurrently_rather_than_buffering() {
    // More than a pipe buffer holds (64 KiB on Linux). Collecting the upstream
    // stage's output before starting the downstream one would deadlock here, so
    // this is the test that an in-shell stage really gets its own process.
    let out = run_with_input("func many() { for i in 1..30000 { puts $i } }\nmany | wc -l\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "29999",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_downstream_stage_closing_the_pipe_ends_an_in_shell_stage_quietly() {
    // `head` exits after three lines, so the writer takes SIGPIPE. Rust sets
    // SIGPIPE to SIG_IGN at startup; if the forked stage inherited that it would
    // see EPIPE, report a write failure, and fail the pipeline instead of ending
    // silently the way the pipefail rule assumes.
    let out = run_with_input("func many() { for i in 1..200000 { puts $i } }\nmany | head -3\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n2\n3\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "upstream SIGPIPE must not fail the pipeline"
    );
}

#[test]
fn an_in_shell_stage_reports_its_status_and_keeps_its_state_to_itself() {
    let out = run_with_input("func fail() { return 3 }\nfail | cat\n");
    assert_eq!(out.status.code(), Some(3));

    // A stage runs in its own process, so what it changes dies with it — the
    // same bargain every POSIX shell makes for a piped builtin.
    let out =
        run_with_input("x = before\nfunc setit() { x = after\nputs done }\nsetit | cat\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\nbefore\n");
}

#[test]
fn a_redirected_function_writes_to_the_target() {
    // A function runs inside the shell, so its `>` is applied to the shell's own
    // stdout for the duration of the call. Output written by the body — including
    // by an external command it runs — lands in the file, and the shell's stdout is
    // restored afterward.
    let dir = fresh_dir("func_redirect");
    let target = dir.join("out");
    let out = run_with_input(&format!(
        "func f() {{ puts hi\n puts there }}\nf > {}\nputs back-on-stdout\n",
        target.display()
    ));
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "back-on-stdout\n");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi\nthere\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirected_function_appends_and_reads_stdin() {
    let dir = fresh_dir("func_redirect_more");
    let target = dir.join("out");
    let input = dir.join("in");
    std::fs::write(&input, "from-the-file\n").unwrap();
    let out = run_with_input(&format!(
        "func w() {{ puts one }}\nw > {t}\nw >> {t}\nfunc r() {{ cat }}\nr < {i}\n",
        t = target.display(),
        i = input.display()
    ));
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    // `>` then `>>` accumulates; `<` feeds the body (here an external `cat`).
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\none\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from-the-file\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirected_functions_arguments_are_expanded_before_the_target_is_opened() {
    // Creating the redirection target must not change what the call's own glob
    // matches — the external-command path builds its argv before opening too. In a
    // directory holding only `input`, `f * > summary` passes just `input`.
    let dir = fresh_dir("func_redirect_glob");
    std::fs::write(dir.join("input"), "").unwrap();
    let out = run_with_input(&format!(
        "cd {}\nfunc f(...xs) {{ puts ...$xs }}\nf * > summary\ncat summary\n",
        dir.display()
    ));
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "input\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unopenable_redirect_target_for_a_function_is_reported() {
    // The open failure is reported with its path and the shell keeps going.
    let out = run_with_input("func f() { puts hi }\nf > /nonexistent-dir/out\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/nonexistent-dir/out"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn an_unterminated_definition_at_eof_is_reported() {
    let out = run_with_input("func f() {\n  puts hi\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unexpected end of input"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn malformed_compound_headers_use_parser_diagnostics() {
    for input in [
        "func f)\nputs after\n",
        "func f() oops\nputs after\n",
        "func f(,)\nputs after\n",
        "for 1\nputs after\n",
    ] {
        let out = run_with_input(input);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{input:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn parser_incomplete_is_the_only_compound_continuation_signal() {
    // The reader no longer guesses whether the next physical line was intended
    // as a body. It buffers while the parser says the whole unit is incomplete,
    // then reports the parser's error for that unit.
    for (input, expected) in [
        ("func f()\nputs after\n", ""),
        ("func f()\nputs '{'\nputs after\n", "after\n"),
    ] {
        let out = run_with_input(input);
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{input:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{input:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn append_in_a_function_does_not_leak_to_the_global() {
    // `g += after` inside a function binds a local (seeded from the visible
    // global), so the global keeps its value after the call returns.
    let out = run_with_input("g = before\nfunc f() { g += after; puts $g }\nf\nputs $g\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "beforeafter\nbefore\n"
    );
}

#[test]
fn an_escaped_newline_before_a_raw_string_still_closes_the_body() {
    // A `\`-newline inside a body is a line boundary, so the raw string on the
    // next line is raw and the body's closing `}` is still found — the definition
    // is accepted and later top-level commands are not swallowed.
    let out = run_with_input("func f() { true \\\nr'\\' }\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("missing closing"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_body_opening_brace_may_sit_on_the_next_line() {
    // `func f()` then `{` on the following line is a valid layout (the grammar's
    // `")" ws? "{"`), so the reader buffers through to the body's `}` and defines
    // the function rather than running the body at top level.
    let out = run_with_input("func f()\n{\n  puts body-ran\n}\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "body-ran\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("command not found"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_delayed_brace_after_a_blank_line_still_defines() {
    // A blank line between the header and its `{` keeps buffering (it does not
    // invalidate the awaited body), so the function is still defined.
    let out = run_with_input("func f()\n\n{\n  puts ok\n}\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn a_multi_line_signature_still_buffers_and_defines() {
    // A valid parameter list split across lines keeps buffering until the `)` and
    // body arrive, then defines normally.
    let out = run_with_input("func add(a,\nb) {\n  puts $a $b\n}\nadd 1 2\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1 2\n");
}

#[test]
fn a_bare_list_reaches_an_in_shell_function_intact() {
    // Per DESIGN.md, an unspread list passes to an in-shell function as one list
    // value — so the parameter holds the whole list and can be spread inside.
    let out = run_with_input("xs = [a b]\nfunc f(x) { puts ...$x }\nf $xs\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_list_argument_counts_as_one_positional() {
    // `f $xs tail` binds the whole list to the first positional and `tail` to the
    // second — a list is one argument, not its elements.
    let out = run_with_input("xs = [a b c]\nfunc f(x, y) { puts ...$x; puts $y }\nf $xs tail\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b c\ntail\n");
}

#[test]
fn a_list_slice_reaches_a_function_as_a_list() {
    let out = run_with_input("xs = [a b c d]\nfunc f(x) { puts ...$x }\nf $xs[1..3]\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b c\n");
}

#[test]
fn a_bare_map_reaches_an_in_shell_function_intact() {
    let out = run_with_input("func show(x) { puts $x.a }\nm = [a: ok]\nshow $m\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn integer_and_boolean_values_remain_typed() {
    let out = run_with_input(
        "n = 40 + 2\n\
         found = $n == 42\n\
         n += 1\n\
         puts $n $found\n\
         puts \"$n:$found\"\n\
         text = \"42\"\n\
         parsed = $text:int + 1\n\
         puts $parsed\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "43 true\n43:true\n43\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn arithmetic_does_not_coerce_strings() {
    let out = run_with_input("n = \"1\" + 2\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("expected integer"));
}

#[test]
fn a_bare_list_to_an_external_command_is_still_an_error() {
    // The external-argv rule is unchanged: a bare list must be spread or joined.
    let out = run_with_input("xs = [a b]\necho $xs\n");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("list value needs"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn path_and_string_modifiers_transform_values_and_chain() {
    let out = run_with_input(
        "file = src/archive.tar.gz\nputs $file:dir $file:base $file:ext $file:exts $file:stem $file:bare\nputs $file:base:upper\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "src archive.tar.gz gz tar.gz archive.tar archive\nARCHIVE.TAR.GZ\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn path_modifiers_handle_relative_leaves_roots_and_dotfiles() {
    let out = run_with_input(
        r#"leaf = report.txt
root = "/"
dot = ".config.toml"
puts $leaf:dir $root:dir
puts $dot:exts $dot:bare
"#,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), ". /\ntoml .config\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn value_modifiers_recurse_through_nested_lists() {
    let out = run_with_input("xs = [[a b] c]\nys = $xs:upper\nputs ...$ys[0]\nputs $ys[1]\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "A B\nC\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn for_rejects_the_reserved_environment_binding() {
    let out = run_with_input("for env in [a] { puts BAD }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved name"));
}

#[test]
fn guard_errors_fail_the_conditional_list() {
    let out = run_with_input("puts BAD if $missing && puts ALSO_BAD\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unbound variable"));
}

#[test]
fn remainder_overflow_is_not_reported_as_division_by_zero() {
    let out = run_with_input("x = (-9223372036854775807 - 1) % -1\n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("numeric overflow"), "{stderr}");
    assert!(!stderr.contains("division by zero"), "{stderr}");
}

#[test]
fn quoted_path_with_spaces_runs_in_command_position() {
    let dir = fresh_dir("quoted command");
    let command = dir.join("say hello");
    std::fs::write(&command, "#!/bin/sh\nprintf 'ran\\n'\n").unwrap();
    let mut permissions = std::fs::metadata(&command).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&command, permissions).unwrap();
    let out = run_with_input(&format!("\"{}\"\n", command.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tilde_expansion_ignores_adjacent_empty_quotes() {
    let home = fresh_dir("tilde_empty_quote");
    let out = run_with_home("puts ~\"\" ~\"\"/child\n", &home);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{} {}/child\n", home.display(), home.display())
    );
}

#[test]
fn captures_command_output_as_an_expression_value() {
    let out = run_with_input("answer = $(printf 20):int + 22\nputs $answer\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn expression_condition_errors_do_not_select_else() {
    let out = run_with_input("if $missing { puts BAD } else { puts ALSO_BAD }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(!out.status.success());
}

#[test]
fn stderr_pipe_connects_to_the_next_stage() {
    let out = run_with_input("sh -c 'echo out; echo err >&2' |& cat\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\nerr\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn background_conditional_lists_are_rejected_as_one_unit() {
    let dir = fresh_dir("background_and_or");
    let marker = dir.join("marker");
    let out = run_with_input(&format!("false && touch {} &\n", marker.display()));
    assert_eq!(out.status.code(), Some(2));
    assert!(!marker.exists());
}

#[test]
fn break_inside_a_function_does_not_continue_its_body() {
    let out = run_with_input("func f() { break; puts BAD }\nf\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(!out.status.success());
}

#[test]
fn multiple_quoted_glob_hyphens_stay_literal() {
    let dir = fresh_dir("multiple_quoted_hyphens");
    for name in ["-", "a", "z"] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    let out = run_with_input(&format!("cd {}\nputs [a'--'z]\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "- a z\n");
}

#[test]
fn command_branch_output_becomes_the_if_expression_value() {
    let out = run_with_input(
        "french = true\ngreeting = if $french { printf bonjour } else { hi }\nputs $greeting\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bonjour\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn malformed_function_bodies_remain_quarantined() {
    for input in [
        "func f(x {\nputs LEAKED\n}\n",
        "func f(x {\nputs )\nputs LEAKED\n}\n",
    ] {
        let out = run_with_input(input);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "");
        assert!(String::from_utf8_lossy(&out.stderr).contains("syntax error"));
    }
}

#[test]
fn interpolated_command_allows_multiple_input_redirects() {
    let dir = fresh_dir("multiple_input_redirects");
    let first = dir.join("first");
    let second = dir.join("second");
    std::fs::write(&first, "first\n").unwrap();
    std::fs::write(&second, "second\n").unwrap();
    let out = run_with_input(&format!(
        "cmd = cat\n$cmd < {} < {}\n",
        first.display(),
        second.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "second\n");
}

#[test]
fn collection_modifiers_preserve_typed_list_results() {
    let out = run_with_input(
        "xs = [a b b c]\nputs $xs:len $xs:first $xs:last\nputs ...$xs:rest:init:dedup\nys = $xs:rest:init\nputs ...$ys\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4 a c\nb\nb b\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn join_and_split_modifiers_take_a_separator_argument() {
    // `:join(SEP)` folds a list to a string; `:split(SEP)` is its inverse. Both
    // must sit in expression position (an assignment right-hand side) today —
    // the command-word tokenizer does not yet carry a modifier's argument list.
    let out = run_with_input(
        "dirs = [/usr/bin /bin]\npath = $dirs:join(\":\")\nputs $path\nfields = $path:split(\":\")\nputs $fields:len\nleaf = $path:split(\":\"):first\nputs $leaf\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/usr/bin:/bin\n2\n/usr/bin\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn split_drops_the_trailing_delimiter_run() {
    // The separator is a terminator, not a separator: a trailing run of empties
    // is dropped, interior empties are kept.
    let out = run_with_input(
        "a = \"x:y:\"\nn = $a:split(\":\"):len\nputs $n\nb = \"x::y\"\nm = $b:split(\":\"):len\nputs $m\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n3\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn split_operates_on_the_trimmed_capture_value() {
    // `:split` is a value modifier for now: a `$(…)` capture has its trailing
    // newline trimmed before the split runs, so the newline is not a field. Raw
    // split-modifier binding (DESIGN.md) lands with the `:lines`/`:nulls` family.
    let out = run_with_input("x = $(printf \"a:\\n\"):split(\":\")\nputs $x:len\nputs ...$x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\na\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn join_of_a_nested_list_fails_loud() {
    let out = run_with_input("xs = [a b]\nys = [$xs c]\nz = $ys:join(\",\")\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot join a nested list"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
}

#[test]
fn unknown_modifier_names_remain_literal_suffixes() {
    let out = run_with_input("host = example\nputs $host:port\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "example:port\n");
}

#[test]
fn a_raw_string_body_immediately_after_the_brace_defines() {
    // `func f(){r'\'}` — a raw string as the first body word with no space after
    // `{`; the body's `}` is still found and the definition is accepted.
    let out = run_with_input("func f(){r'\\'}\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("missing closing")
            && !String::from_utf8_lossy(&out.stderr).contains("unexpected text"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn for_iterates_lists_without_word_splitting() {
    let out = run_with_input("xs = [one \"two words\" three]\nfor x in $xs { puts \"<$x>\" }\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "<one>\n<two words>\n<three>\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn for_iterates_direct_list_literals_and_skips_an_empty_literal() {
    let out =
        run_with_input("for x in [a \"b c\"] { puts \"<$x>\" }\nfor x in [] { puts never }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<a>\n<b c>\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn for_iterates_integer_ranges_and_ordered_maps() {
    let out = run_with_input(
        "for i in 1..4 { puts $i }\n\
         for i in 2..=4 { puts $i }\n\
         for i in 4..2 { puts never }\n\
         ports = [http: 80, https: 443]\n\
         for protocol, port in $ports { puts \"$protocol=$port\" }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n2\n3\n2\n3\n4\nhttp=80\nhttps=443\n"
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn for_reports_binder_and_range_type_errors_and_recovers() {
    let out = run_with_input(
        "for key in [key: value] { puts never }\n\
         for left, right in [a b] { puts never }\n\
         for i in 1..word { puts never }\n\
         puts recovered\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "recovered\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("map iteration requires"), "{stderr}");
    assert!(
        stderr.contains("two loop bindings require a map"),
        "{stderr}"
    );
    assert!(
        stderr.contains("range endpoints must be integers"),
        "{stderr}"
    );
}

#[test]
fn a_top_level_return_in_a_for_body_does_not_skip_the_iteration() {
    let out = run_with_input("for x in [a b] { return; puts $x }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr)
            .matches("return: not inside a function")
            .count(),
        2
    );
}

#[test]
fn for_supports_multiline_bodies_and_empty_lists() {
    let out = run_with_input(
        "xs = [a b]\nseen = \"\"\nfor x in $xs {\n  puts $x\n  seen += $x\n}\nempty = []\nfor x in $empty { puts never }\nputs $seen\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\nab\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn parsed_source_sequences_expression_assignments() {
    let out = run_with_input("answer = 20 + 22; puts $answer\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn parsed_command_words_and_redirects_keep_quote_structure() {
    let dir = fresh_dir("parsed_word_redirect");
    let path = dir.join("result.txt");
    let input = format!(
        "target = {}\nfor item in [once] {{ /bin/echo \"*\" > $target }}\n",
        path.display()
    );

    let out = run_with_input(&input);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "*\n");
}

#[test]
fn break_controls_a_parsed_loop_body() {
    let out = run_with_input("for x in [a b c] {\nputs $x\nbreak\nputs never\n}\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn nested_loop_control_targets_the_nearest_loop_through_if() {
    let out = run_with_input(
        "for outer in [a b] {\n\
           for inner in [1 2 3] {\n\
             if $inner == 2 { continue }\n\
             puts $outer $inner\n\
             if $inner == 3 { break }\n\
           }\n\
           puts done-$outer\n\
         }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a 1\na 3\ndone-a\nb 1\nb 3\ndone-b\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn loop_control_stops_a_function_body_without_leaking_to_the_callers_loop() {
    let out = run_with_input(
        "func stop() { break; puts BAD }\n\
         func skip() { continue; puts BAD }\n\
         for item in [a b] {\n\
           if $item == a { skip }\n\
           puts seen-$item\n\
           stop\n\
           puts after-stop\n\
         }\n\
         puts finished\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "seen-a\nafter-stop\nseen-b\nafter-stop\nfinished\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn maps_preserve_order_support_access_spread_and_merge() {
    let out = run_with_input(
        "key = https\n\
         ports = [http: 80, https: 443, http: 8080]\n\
         puts $ports.http ${ports[$key]}\n\
         defaults = [ssh: 22, http: 80]\n\
         ports += $defaults\n\
         copy = [...$ports, ssh: 2222]\n\
         puts ...$copy:keys\n\
         puts ...$copy:values\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "8080 443\nhttp https ssh\n80 443 2222\n"
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn maps_reject_missing_keys_and_non_string_keys() {
    let missing = run_with_input("m = [present: yes]\nputs $m.absent\n");
    assert_eq!(missing.status.code(), Some(1));
    // A permanent error, not an unimplemented feature: it used to render through
    // `Unsupported`, which appends "not supported yet".
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("$m: no `absent` in this map"), "{stderr:?}");
    assert!(!stderr.contains("not supported yet"), "{stderr:?}");

    let bad_key = run_with_input("keys = [bad]\nm = [$keys: value]\n");
    assert_eq!(bad_key.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad_key.stderr).contains("map key must be a string"));
}

#[test]
fn command_interpolation_dispatches_map_subscripts_by_value_type() {
    let out = run_with_input(
        "m = [200: numeric, \"a b\": quoted, x: dynamic]\n\
         key = x\n\
         puts $m[200] ${m[\"a b\"]} $m[$key]\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "numeric quoted dynamic\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn command_interpolation_resolves_chained_map_members_in_order() {
    let out = run_with_input(
        "inner = [key: value]\n\
         outer = [inner: $inner]\n\
         puts $outer.inner.key\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "value\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn list_patterns_bind_names_discards_and_a_middle_rest_atomically() {
    let out = run_with_input(
        "[first ...middle last] = [a b c d]\n\
         [_ kept] = [ignored yes]\n\
         puts $first ...$middle $last $kept\n\
         first = unchanged\n\
         [first missing] = [only]\n\
         puts $first\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a b c d yes\nunchanged\n"
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not match binding pattern"));
}

#[test]
fn conditional_list_binding_skips_mismatches_without_partial_updates() {
    let out = run_with_input(
        "a = old\n\
         if [a b] = [one] { puts wrong } else { puts $a }\n\
         if [head ...tail] = [one two three] { puts $head ...$tail }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "old\none two three\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn loops_and_match_arms_share_list_pattern_binding() {
    let out = run_with_input(
        "rows = [[a b] [c d]]\n\
         for [left right] in $rows { puts $left $right }\n\
         result = match [start x y] {\n\
           [verb ...args] if $verb == start { [$verb ...$args] }\n\
           _ { [wrong] }\n\
         }\n\
         puts ...$result\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a b\nc d\nstart x y\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn match_uses_ordered_literal_glob_regex_range_and_alternative_arms() {
    let out = run_with_input(
        "kind = match README.md {\n\
           *.txt { text }\n\
           *.md | *.markdown { markdown }\n\
           _ { other }\n\
         }\n\
         number = match 7 { 1..=9 { digit } _ { other } }\n\
         exact = match 42 { 42 { integer } _ { wrong } }\n\
         regex = match README.md { /^README/ { readme } _ { wrong } }\n\
         first = match file.txt { * { broad } *.txt { narrow } }\n\
         puts $kind $number $exact $regex $first\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "markdown digit integer readme broad\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn match_statement_runs_for_effect_and_guards_continue_to_later_arms() {
    let out = run_with_input(
        "match [skip payload] {\n\
           [verb value] if $verb == take { puts wrong }\n\
           [verb value] if $value == payload { puts $verb $value }\n\
           _ { puts wrong }\n\
         }\n\
         empty = match absent { present { wrong } }\n\
         puts \"<$empty>\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "skip payload\n<>\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tilde_matches_regexes_and_globs() {
    let out = run_with_input(
        "digits = item42 ~ /\\d+$/\n\
         slash = a/b ~ /a\\/b/\n\
         file = src/main.rs ~ src/*.rs\n\
         insensitive = ERROR ~ /error/:i\n\
         negative = notes.txt !~ *.rs\n\
         puts $digits $slash $file $insensitive $negative\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true true true true true\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn re_constructs_reusable_and_literal_patterns() {
    let out = run_with_input(
        "pattern = re(r'^a.c$')\n\
         dynamic = abc ~ $pattern\n\
         exact = a.c ~ re('a.c', literal: true)\n\
         puts $dynamic $exact\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true true\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tilde_rejects_quoted_and_invalid_regex_patterns() {
    let out = run_with_input(
        "bad = abc ~ 'a.c'\n\
         broken = abc ~ /\\k/\n\
         puts after\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("right operand of `~` must be a regex or bare glob"));
    assert!(stderr.contains("invalid regex"));
}

// ---------------------------------------------------------------------------
// Invocation (`mesh SCRIPT`, `-c`, `-s`, `$sh.args` / `$sh.name`)
// ---------------------------------------------------------------------------

/// Run mesh with command-line arguments and no piped stdin.
fn run_with_args(args: &[&str]) -> Output {
    mesh_command()
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run mesh")
}

/// Write `body` into a fresh script file and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let path = fresh_dir(tag).join("script.mesh");
    std::fs::write(&path, body).expect("write script");
    path
}

#[test]
fn runs_a_script_file_named_on_the_command_line() {
    let path = script("run_script", "puts one\nputs two\n");
    let out = run_with_args(&[path.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\ntwo\n");
    assert!(out.status.success());
}

#[test]
fn a_script_reads_its_arguments_from_sh_args() {
    let path = script(
        "script_args",
        "puts $sh.args:len\nputs $sh.args[0]\nputs ...$sh.args\n",
    );
    let out = run_with_args(&[path.to_str().unwrap(), "one", "two three"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2\none\none two three\n"
    );
}

#[test]
fn sh_name_is_the_script_and_sh_args_is_empty_without_operands() {
    let path = script("script_name", "puts $sh.name\nputs $sh.args:len\n");
    let out = run_with_args(&[path.to_str().unwrap()]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n0\n", path.display())
    );

    let out = run_with_input("puts $sh.name\nputs $sh.args:len\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "mesh\n0\n");
}

#[test]
fn option_parsing_stops_at_the_script_so_its_flags_reach_it() {
    let path = script("script_flags", "puts ...$sh.args\n");
    let out = run_with_args(&[path.to_str().unwrap(), "--login", "-c", "x"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "--login -c x\n");
}

#[test]
fn a_script_exit_status_is_its_last_command_or_an_explicit_exit() {
    let path = script("script_status", "false\n");
    assert_eq!(
        run_with_args(&[path.to_str().unwrap()]).status.code(),
        Some(1)
    );

    let path = script("script_exit", "exit 3\nputs unreachable\n");
    let out = run_with_args(&[path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
}

#[test]
fn a_syntax_error_rejects_the_whole_script_before_anything_runs() {
    let path = script("script_syntax", "puts before\nresult = 1 < 2 < 3\n");
    let out = run_with_args(&[path.to_str().unwrap()]);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("comparisons cannot be chained"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn an_unreadable_script_reports_the_path_with_a_command_like_status() {
    let dir = fresh_dir("script_missing");
    let missing = dir.join("nope.mesh");
    let out = run_with_args(&[missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(127));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("mesh: {}:", missing.display())),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A directory exists but cannot be read as a script: not-found's 127 would
    // be a lie, so it takes 126 — "found, but not runnable".
    let out = run_with_args(&[dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(126));
}

#[test]
fn a_script_runs_from_a_shebang_line() {
    let path = script(
        "script_shebang",
        &format!(
            "#!{}\nputs \"hi $sh.args[0]\"\n",
            env!("CARGO_BIN_EXE_mesh")
        ),
    );
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&path, permissions).unwrap();

    let out = Command::new(&path)
        .arg("world")
        .env("XDG_CONFIG_HOME", isolated_config_home())
        .stdin(Stdio::null())
        .output()
        .expect("run script through its shebang");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi world\n");
}

#[test]
fn dash_c_runs_a_command_string_with_its_own_arguments() {
    let out = run_with_args(&["-c", "puts hi\nputs ...$sh.args\n", "a", "b"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\na b\n");
    assert!(out.status.success());

    // The command string keeps the shell's own name; only a script renames it.
    let out = run_with_args(&["-c", "puts $sh.name"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "mesh\n");
}

#[test]
fn dash_c_and_dash_s_require_and_consume_the_right_operands() {
    let out = run_with_args(&["-c"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("-c requires a command string"));

    // `-s` reads stdin and takes the remaining operands as arguments.
    let mut child = mesh_command()
        .args(["-s", "p", "q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"puts ...$sh.args\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "p q\n");
}

#[test]
fn a_double_dash_ends_options_without_becoming_the_script() {
    let path = script("script_ddash", "puts ...$sh.args\n");
    let out = run_with_args(&["--", path.to_str().unwrap(), "--norc"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "--norc\n");
}

#[test]
fn help_and_version_print_and_exit_successfully() {
    let out = run_with_args(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("Usage: mesh"), "{stdout}");
    assert!(stdout.contains("-c COMMAND"), "{stdout}");

    let out = run_with_args(&["--version"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("mesh "),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_with_args(&["--nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown option `--nope`"));
}

#[test]
fn sh_is_a_reserved_namespace_that_cannot_be_bound() {
    let out = run_with_input("sh = 1\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved name"));

    let out = run_with_input("func f(sh) { puts $sh }\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved"));
}

// ---------------------------------------------------------------------------
// The `$env` namespace (`$env.KEY = value`)
// ---------------------------------------------------------------------------

#[test]
fn assigning_env_sets_it_for_this_shell_and_for_children() {
    let out = run_with_input("$env.MESH_TEST_TOOL = ready\nputs $env.MESH_TEST_TOOL\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ready\n");

    // The point of the environment is what children inherit, so check one.
    let out = run_with_input("$env.MESH_TEST_TOOL = ready\n/usr/bin/env\n");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == "MESH_TEST_TOOL=ready"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn an_env_write_inside_a_function_persists_after_it_returns() {
    // Export is deliberately a global effect, not a local-by-default binding:
    // changing what children inherit is the whole point.
    let out = run_with_input(
        "func setup() { $env.MESH_TEST_SETUP = done }\nsetup\nputs $env.MESH_TEST_SETUP\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}

#[test]
fn appending_to_a_plain_env_entry_concatenates_strings() {
    let out =
        run_with_input("$env.MESH_TEST_S = ab\n$env.MESH_TEST_S += cd\nputs $env.MESH_TEST_S\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "abcd\n");

    // Appending to an unset name starts it rather than failing.
    let out = run_with_input("$env.MESH_TEST_NEW += first\nputs $env.MESH_TEST_NEW\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "first\n");
}

#[test]
fn path_type_env_entries_are_lists() {
    let out = run_with_input(
        "$env.PATH = [/a /b /a]\n\
         puts $env.PATH:len\n\
         puts $env.PATH[0]\n\
         puts ...$env.PATH[1..]\n\
         puts ...$env.PATH:dedup\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3\n/a\n/b /a\n/a /b\n"
    );
}

#[test]
fn appending_to_path_adds_an_entry_and_children_see_the_joined_value() {
    let dir = fresh_dir("env_path_append");
    let tool = dir.join("mesh-test-tool");
    std::fs::write(&tool, "#!/bin/sh\necho found me\n").unwrap();
    let mut permissions = std::fs::metadata(&tool).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&tool, permissions).unwrap();

    // The payoff: a directory added to $env.PATH is searched for commands.
    let out = run_with_input(&format!(
        "$env.PATH += {}\nputs $env.PATH[-1]\nmesh-test-tool\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\nfound me\n", dir.display()),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_splitting_is_exact_so_empty_components_survive() {
    // `PATH=/a:` means "…and the cwd", so an empty component is meaningful and
    // a split/join round trip has to be byte-faithful.
    let out = run_with_input("$env.PATH = [/a \"\" /b]\n/usr/bin/env\n");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == "PATH=/a::/b"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_with_input("$env.PATH = [/a \"\" /b]\nputs $env.PATH:len\nputs $env.PATH[1]\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n\n");
}

#[test]
fn only_strings_cross_into_the_environment() {
    let out = run_with_input("$env.MESH_TEST_L = [a b]\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("join the list first"), "{stderr}");

    let out = run_with_input("$env.MESH_TEST_M = [a: 1]\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a map"));

    // An environment entry is NUL-terminated, so an embedded NUL is a hard
    // error rather than a silent truncation.
    let out = run_with_input("$env.MESH_TEST_N = \"a\\u{0}b\"\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("NUL"));
}

#[test]
fn a_joined_list_crosses_into_a_plain_env_entry() {
    let out = run_with_input(
        "dirs = [/a /b]\n$env.MESH_TEST_J = $dirs:join(\":\")\nputs $env.MESH_TEST_J\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/a:/b\n");
}

#[test]
fn only_a_plain_env_member_is_an_assignment_target() {
    // An index or a modifier describes a derived value, not a place, so these
    // stay expressions and fail as such rather than silently assigning.
    for source in ["$env.PATH[0] = x\n", "$env.PATH:dedup = x\n", "$env = x\n"] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(2), "{source}");
    }

    // The braced spelling is the same reference, so it is assignable.
    let out = run_with_input("${env.MESH_TEST_B} = ok\nputs $env.MESH_TEST_B\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
}

#[test]
fn reading_an_unset_env_entry_is_still_a_loud_error() {
    let out = run_with_input("puts $env.MESH_TEST_ABSENT\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not set"));
}

#[test]
fn any_env_name_that_can_be_read_can_also_be_assigned() {
    // The environment permits an interior hyphen and `$env.MY-VAR` reads it, so
    // assignment has to accept exactly the names reads do.
    let out = run_with_input("$env.MESH-TEST-KEBAB = ok\nputs $env.MESH-TEST-KEBAB\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = mesh_command()
        .env("MESH-TEST-INHERITED", "from-parent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"puts $env.MESH-TEST-INHERITED\n")?;
            child.wait_with_output()
        })
        .expect("run mesh");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from-parent\n");
}

#[test]
fn appending_preserves_non_utf8_bytes_already_in_the_environment() {
    // Environment values are arbitrary non-NUL bytes. Decoding the current value
    // into a mesh string to append would replace an invalid sequence with U+FFFD
    // and write the mangled bytes back, silently breaking a PATH entry that had
    // been resolving fine.
    let weird = std::ffi::OsString::from_vec(b"/usr/bin:/x\xffy".to_vec());
    let out = mesh_command()
        .env("PATH", &weird)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"$env.PATH += /new\n/usr/bin/env\n")?;
            child.wait_with_output()
        })
        .expect("run mesh");
    assert!(
        out.stdout
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"PATH=/usr/bin:/x\xffy:/new"),
        "the original bytes should survive: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The same for a plain (non-path) entry, which concatenates instead.
    let weird = std::ffi::OsString::from_vec(b"a\xffb".to_vec());
    let out = mesh_command()
        .env("MESH_TEST_RAW", &weird)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"$env.MESH_TEST_RAW += Z\n/usr/bin/env\n")?;
            child.wait_with_output()
        })
        .expect("run mesh");
    assert!(
        out.stdout
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"MESH_TEST_RAW=a\xffbZ"),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------------------
// `while` and `loop`
// ---------------------------------------------------------------------------

#[test]
fn while_repeats_until_its_condition_fails() {
    let out = run_with_input("i = 0\nwhile $i < 3 {\n  puts $i\n  i = $i + 1\n}\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "0\n1\n2\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A false condition runs the body zero times.
    let out = run_with_input("while false { puts never }\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn while_accepts_a_command_status_as_its_condition() {
    // The same two condition forms `if` takes: a value's truthiness, or a
    // command's exit status.
    let out = run_with_input("n = 0\nwhile test $n -lt 2 {\n  puts \"n=$n\"\n  n = $n + 1\n}\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "n=0\nn=1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn loop_repeats_until_a_break() {
    let out = run_with_input("i = 0\nloop {\n  i = $i + 1\n  if $i > 2 { break }\n  puts $i\n}\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n2\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn break_and_continue_work_in_the_new_loops() {
    let out = run_with_input(
        "i = 0\nwhile $i < 5 {\n  i = $i + 1\n  if $i == 2 { continue }\n  puts $i\n}\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n3\n4\n5\n");

    // A `break` leaves only the innermost loop.
    let out = run_with_input(
        "for a in [x y] {\n  i = 0\n  while $i < 3 { i = $i + 1\n    if $i == 2 { break } }\n  puts \"$a done\"\n}\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x done\ny done\n");
}

#[test]
fn a_return_inside_a_while_unwinds_the_whole_function() {
    let out = run_with_input(
        "func f() {\n  i = 0\n  while $i < 9 { i = $i + 1\n    if $i == 3 { return 7 } }\n  puts unreachable\n}\nf\n",
    );
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn a_while_reports_the_status_of_its_last_pass() {
    let out = run_with_input("i = 0\nwhile $i < 2 { i = $i + 1\n  false }\n");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn a_spaced_comparison_in_a_condition_is_not_a_redirection() {
    // `<` and `>` double as redirect operators, so a condition has to tell
    // `if $i < 3` from `if cmd < file`. `<=`, `>=`, and `!=` always read as
    // comparisons here; these two now do too.
    let out =
        run_with_input("i = 0\nif $i < 3 { puts lt }\nif $i > 9 { puts gt } else { puts le }\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "lt\nle\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A command condition still redirects.
    let dir = fresh_dir("condition_redirect");
    let input = dir.join("in.txt");
    std::fs::write(&input, "hello\n").unwrap();
    let out = run_with_input(&format!(
        "if grep -q hello < {} {{ puts found }}\n",
        input.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "found\n");
    let _ = std::fs::remove_dir_all(&dir);
}
