//! End-to-end tests that drive the built `mesh` binary.
//!
//! No test-harness crates: Cargo exposes the binary path as `CARGO_BIN_EXE_mesh`
//! to integration tests, so std is enough. Input is piped on stdin (making the
//! shell non-interactive, so no prompt is written), and we assert on stdout,
//! stderr, and the exit code.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
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
fn a_descriptor_redirect_is_rejected_for_now() {
    // `2>err` and `&>f` are deferred descriptor redirects — rejected loudly, not
    // silently reinterpreted as a stdout redirect with a stray `2`/`&` argument.
    let out = run_with_input("echo hello 2>err\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("descriptor redirection"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    for command in ["true;2>err", "true&&2>err", "false||2>err", "echo x|2>err"] {
        let boundary = run_with_input(&format!("{command}\nputs after\n"));
        assert!(
            String::from_utf8_lossy(&boundary.stderr).contains("descriptor redirection"),
            "{command:?} should reject the descriptor redirect"
        );
        assert_eq!(String::from_utf8_lossy(&boundary.stdout), "after\n");
    }
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
    let out = run_with_input(&format!(
        "/bin/echo ok > {} &\n/bin/sleep 0.05\njobs\n",
        missing.display()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Done (1)"), "{stderr}");
    assert!(stderr.contains(&format!("mesh: {}:", missing.display())));
    assert!(!stderr.contains("mesh-redir"));
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
fn a_function_in_a_pipeline_is_rejected() {
    let out = run_with_input("func f() { puts hi }\nf | cat\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not supported in a pipeline"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_redirected_function_is_rejected() {
    let dir = fresh_dir("func_redirect");
    let target = dir.join("out");
    let out = run_with_input(&format!(
        "func f() {{ puts hi }}\nf > {}\n",
        target.display()
    ));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("redirection or backgrounding of a function"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!target.exists());
    let _ = std::fs::remove_dir_all(&dir);
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
    assert!(String::from_utf8_lossy(&missing.stderr).contains("map key"));

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
