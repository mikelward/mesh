//! End-to-end tests that drive the built `mesh` binary.
//!
//! No test-harness crates: Cargo exposes the binary path as `CARGO_BIN_EXE_mesh`
//! to integration tests, so std is enough. Input is piped on stdin (making the
//! shell non-interactive, so no prompt is written), and we assert on stdout,
//! stderr, and the exit code.

use std::io::Write;
use std::os::fd::{FromRawFd, RawFd};
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
        Self::with_environment(config_home, &[])
    }

    /// Extra `NAME=value` entries, replacing any the test process inherited.
    /// `TERM` needs this: what a shell writes for the terminal depends on it, and
    /// what the test runner was started with is not a fact the tests can assume.
    fn with_environment(config_home: &Path, extra: &[(&str, &str)]) -> Self {
        use std::ffi::CString;

        let path = CString::new(env!("CARGO_BIN_EXE_mesh")).unwrap();
        let arguments = [CString::new("mesh").unwrap()];
        let argv = [arguments[0].as_ptr(), std::ptr::null()];

        let mut environment: Vec<_> = std::env::vars_os()
            // Two variables decide *what the shell writes* rather than how it
            // behaves, so inheriting either makes an assertion depend on how the
            // suite was launched. Never inherited; a test that wants one passes it
            // explicitly.
            //
            // `TERM_PROGRAM` picks the shell-integration dialect, so a suite run
            // from inside VS Code's terminal would flip every `OSC 133` assertion to
            // `OSC 633`. `NO_COLOR` suppresses a styled value's attributes, so a
            // suite run with it set would see no SGR at all.
            .filter(|(name, _)| {
                name != "XDG_CONFIG_HOME" && name != "TERM_PROGRAM" && name != "NO_COLOR"
            })
            .filter(|(name, _)| !extra.iter().any(|(replaced, _)| name == replaced))
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
        for (name, value) in extra {
            environment.push(CString::new(format!("{name}={value}")).unwrap());
        }

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

/// Script line that blocks until job `id` has finished, *without* consuming it.
///
/// The tests using this assert on the shell's `[N] Done (…)` notice, or on a
/// `jobs` listing that should already be empty. Both come from the prompt-time
/// reap, so `wait` cannot serve here however much it looks like the right verb:
/// it takes the job out of the table, and the notice never comes. Polling the
/// job's own state stops at the same instant without removing anything.
///
/// This replaces a fixed sleep, which is not merely less tidy but wrong: an
/// interval picked to outlast the job — 0.3s for a job that sleeps 0.05s — still
/// lost under load, and the failure is a *missing* `Done` line rather than a
/// late one, so no amount of generosity closes it.
///
/// The id is a parameter because assuming 1 is a live hazard: launching a
/// second background job reaps the finished first one, and the new job is 2.
/// Waiting on 1 there reads a job that is gone — `no \`1\` in this map` — and
/// returns at once, so the wait silently stops being one.
fn await_job(id: u8) -> String {
    format!("while $sh.jobs[{id}].state == running {{ sleep 0.02 }}\n")
}

/// `openpty` plus `FD_CLOEXEC` on both ends.
///
/// Without the flag these descriptors are inherited by every process the suite
/// spawns *and survive its `exec`* — only `dup2` and an explicit close clear it.
/// That leaks into unrelated tests rather than staying here: a child exec'd
/// while a PTY test holds descriptor 3 starts with 3 already taken, and
/// `run_at_descriptor_limit`'s `RLIMIT_NOFILE` of 4 then leaves the dynamic
/// loader no descriptor to open a shared library with. It fails before `main`
/// runs, with `error while loading shared libraries: … Error 24` — `EMFILE` —
/// which reads as the shell declining a redirection it was never asked about.
///
/// The harnesses hand the slave to their child through `dup2`, which clears the
/// flag on the copy, so nothing that should be inherited stops being.
fn open_pty_pair(master: &mut i32, slave: &mut i32) -> i32 {
    let ok = unsafe {
        // `null_mut` rather than `null` for the termios/winsize arguments: Apple
        // declares them `*mut` and Linux `*const`, and `*mut T` coerces to
        // `*const T` but not the reverse — so only this spelling builds on both.
        libc::openpty(
            master,
            slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        for fd in [*master, *slave] {
            // SAFETY: both descriptors were just returned by `openpty`.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
    ok
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

/// Run mesh with `RLIMIT_STACK` lowered to `bytes`, to reach the cases that only
/// happen on a stack smaller than the usual one.
///
/// The limit is applied in the child between `fork` and `exec` — `pre_exec` — so
/// it lands on mesh and not on the test process, which needs its own stack intact
/// to go on running the rest of the suite.
fn run_with_input_and_stack_limit(input: &str, bytes: u64) -> Output {
    let mut command = mesh_command();
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    // SAFETY: `setrlimit` is async-signal-safe, which is the bar for anything run
    // between `fork` and `exec` in a process that may be threaded.
    unsafe {
        command.pre_exec(move || {
            // Cast rather than take an `rlim_t` argument: the type is `u64` on
            // some targets and `i64` on others, so spelling it at the call site
            // would make callers unportable too.
            let limit = libc::rlimit {
                rlim_cur: bytes as libc::rlim_t,
                rlim_max: bytes as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_STACK, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for mesh")
}

/// Run mesh in its own session, so it has no controlling terminal at all.
///
/// Whether the test runner has one is not something the suite can assume — it does
/// under `cargo test` in a terminal and does not in CI — and `/dev/tty` is exactly
/// the difference. `setsid` makes the answer the same either way.
fn run_without_a_terminal(input: &str) -> Output {
    let mut command = mesh_command();
    // SAFETY: `setsid` between fork and exec is async-signal-safe, and the child
    // is never a process-group leader here, so the call cannot fail for that.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
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
fn notify_needs_a_terminal_and_says_so() {
    // Same contract as `clip`, and the same reason for `setsid`: the sequence is a
    // message to the terminal, so with none there is nowhere to send it — and
    // nothing may reach stdout, which would corrupt a pipeline and still notify
    // nobody.
    let out = run_without_a_terminal("notify hi\nputs status=$sh.status\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mesh: notify: no terminal"),
        "stderr was {stderr:?}"
    );
    assert!(stdout.contains("status=1"), "stdout was {stdout:?}");
    assert!(!stdout.contains("\x1b]9"), "stdout was {stdout:?}");
}

#[test]
fn notify_refuses_an_empty_message() {
    // A notification with nothing in it is a mistake being reported as success:
    // most likely a variable that expanded to nothing.
    let out = run_without_a_terminal("notify \"\"\nputs status=$sh.status\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mesh: notify: nothing to say"),
        "stderr was {stderr:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("status=1"),
        "stdout was {:?}",
        out.stdout
    );
}

#[test]
fn clip_needs_a_terminal_and_says_so() {
    // The sequence is a message to the terminal, so with no terminal there is
    // nowhere to send it — and nothing is written to stdout, which is the mistake
    // worth guarding: an escape on stdout would corrupt a pipeline and still not
    // reach any clipboard.
    let out = run_without_a_terminal("clip hi\nputs status=$sh.status\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mesh: clip: no terminal"),
        "stderr was {stderr:?}"
    );
    assert!(stdout.contains("status=1"), "stdout was {stdout:?}");
    assert!(!stdout.contains("\x1b]52"), "stdout was {stdout:?}");
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
fn a_bash_builtin_mesh_renames_points_at_meshs_spelling() {
    // There is no external `read`, so the bash reflex would otherwise dead-end
    // on a bare `command not found` with nothing to try next.
    let out = run_with_input("read line\n");
    assert_eq!(out.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command not found: read (mesh spells this `gets`"),
        "{stderr}"
    );
}

#[test]
fn a_bash_builtin_with_no_mesh_command_spells_out_the_replacement() {
    let out = run_with_input("local x = 5\n");
    assert_eq!(out.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command not found: local (a plain `x = 5`"),
        "{stderr}"
    );
}

#[test]
fn an_ordinary_missing_command_keeps_the_bare_message() {
    let out = run_with_input("this_command_does_not_exist_42\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command not found: this_command_does_not_exist_42\n"),
        "{stderr}"
    );
}

#[test]
fn command_runs_the_program_past_a_function_of_the_same_name() {
    // The bare name is the function, as always; `command` is how the program it
    // wraps is still reachable, which is what makes `func ls() { ls --color }`
    // safe to write.
    let out = run_with_input("func echo(word) { puts mine }\necho hi\ncommand echo hi\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "mine\nhi\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn command_looks_past_a_builtin_and_says_so_when_there_is_no_program() {
    // `command` is defined to look for a program, so a builtin's name is not
    // found — and a bare "command not found: puts" would read as a lie about a
    // name `help` lists.
    let out = run_with_input("command puts hi\n");
    assert_eq!(out.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "command not found: puts (`puts` is a builtin; `command` looks for a program)"
        ),
        "{stderr}"
    );
}

#[test]
fn commands_own_words_are_only_the_ones_in_front_of_the_program() {
    // Everything from the program name on belongs to the program: `--help` after
    // it is the program's question, which is the whole point of the builtin.
    let out = run_with_input("func printf(x) { puts mine }\ncommand printf '%s\\n' --help\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "--help\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Its own `--help` is the first word after it, and prints mesh's help.
    let out = run_with_input("command --help\n");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Usage: command [--] NAME [ARG ...]"),
        "{stdout}"
    );

    // `--` ends those options and is consumed, so the word after it is the
    // program however it reads.
    let out = run_with_input("command -- echo dashed\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "dashed\n");

    // Nothing to run is an error rather than a silent success, since a `command`
    // with no program in it is a line that lost its command.
    let out = run_with_input("command\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("command: expected a program to run"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_flag_in_front_of_the_program_is_commands_own() {
    // The bash reflex. Reading `-v` as a program name would answer "command not
    // found: -v" — true, and about the wrong question — and would then be the
    // meaning `command -v` had to keep once the option is built.
    let out = run_with_input("command -v ls\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command: -v: asking what a name would run is not built yet"),
        "{stderr}"
    );

    // Any other flag-looking word is simply not an option of `command`'s.
    let out = run_with_input("command --hepl ls\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("command: --hepl: not an option of `command`"),
        "{stderr}"
    );

    // Both messages point at the escape, and it works: after `--` the word is the
    // program, so this is an ordinary "no such program" about `-v` itself.
    let out = run_with_input("command -- -v\n");
    assert_eq!(out.status.code(), Some(127));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("command not found: -v"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_command_stage_is_the_program_itself() {
    // Piped, redirected, backgrounded: the prefix comes off before the stage is
    // built, so each of these is the program's own process rather than a forked
    // shell that then runs it.
    let out = run_with_input("func tr() { puts mine }\ncommand echo piped | command tr a-z A-Z\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "PIPED\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dir = fresh_dir("command_stage");
    let target = dir.join("out.txt");
    let out = run_with_input(&format!(
        "command echo redirected > {}\n",
        target.to_string_lossy()
    ));
    assert!(out.status.success(), "{:?}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap_or_default(),
        "redirected\n"
    );

    // The job is the program, so that is what the listing names — a `command`
    // there would mean the shell had forked itself to run one.
    let out = run_with_input(
        "command sleep 0.2 &\nwhile $sh.jobs[1].state == running { /bin/sleep 0.02 }\njobs\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("] Done (0) sleep 0.2")),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_deferred_command_stage_joins_the_job_table_as_the_program() {
    // A value argument defers the stage, which registers its words *before* the
    // child expands them. Listing that job under `command` would make `%sleep`
    // find nothing, so the same line would be waitable one way and not the other.
    let out = run_with_input("command sleep $(printf 0.3) &\njobs\nwait %sleep\nputs $sh.status\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The listing names the program, not the prefix — a value still shows as
    // `$(…)`, since printing it would run the work the deferral moved.
    assert!(
        stdout.lines().any(|line| line.contains("] Running sleep ")),
        "{stdout}{stderr}"
    );
    assert!(!stdout.contains("Running command"), "{stdout}");
    // And `%sleep` finds it, exactly as it does for the undeferred spelling.
    assert!(stdout.ends_with("0\n"), "{stdout}{stderr}");
}

#[test]
fn a_failed_exec_reports_itself_rather_than_writing_to_a_target() {
    // `Command::spawn` makes a private close-on-exec pipe for the child to
    // report an `exec` failure on. It takes a low descriptor, and installing a
    // stage's own descriptors overwrote it: `missing 4> out` put Rust's binary
    // error packet into `out` and exited 1, with no `command not found`. The
    // stage `execvp`s itself now, so it reports 126/127 from its own process and
    // there is no private pipe to clobber.
    let dir = fresh_dir("exec_failure");
    let out = run_with_input(&format!(
        "this_command_does_not_exist_42 4> {}\nputs after\n",
        dir.join("target.txt").to_string_lossy()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("command not found"), "{stderr}");
    assert_eq!(
        std::fs::read(dir.join("target.txt")).unwrap_or_default(),
        b"",
        "the failure was written into the redirection target"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // Reported by the stage, so its own redirections apply to the report — the
    // same place bash puts it.
    let out = run_with_input(&format!(
        "this_command_does_not_exist_42 2> {}\nputs after\n",
        dir.join("log.txt").to_string_lossy()
    ));
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(dir.join("log.txt"))
            .unwrap_or_default()
            .contains("command not found"),
        "the redirection did not carry the report"
    );

    // A file that exists but cannot be executed is `126`, still from the stage.
    let unrunnable = dir.join("unrunnable");
    std::fs::write(&unrunnable, "not executable\n").expect("write the file");
    let out = run_with_input(&format!("{}\n", unrunnable.to_string_lossy()));
    assert_eq!(out.status.code(), Some(126), "{:?}", out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
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
           if true { fail 7 }\n\
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
    // An external command, because `puts` is a builtin looking at real values and
    // renders a list rather than refusing it.
    let out = run_with_input("xs = [[one two]]\n/bin/echo ...$xs\n");
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
fn print_writes_the_same_text_without_a_trailing_newline() {
    let out = run_with_input("print hello   world\nprint '!'\nputs\nprint\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world!\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn puts_renders_a_list_one_element_per_line() {
    // A list *is* a sequence of lines, so newline is the separator. `puts` can
    // answer this where the argv boundary cannot: it is a builtin holding a real
    // value (`DESIGN.md` §"I/O").
    let out = run_with_input("xs = [a 'b c' d]\nputs $xs\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb c\nd\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn puts_renders_a_map_as_key_value_lines() {
    let out = run_with_input("m = [host: build1, port: 22]\nputs $m\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "host: build1\nport: 22\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn puts_keeps_the_order_of_a_mixed_argument_list() {
    // The rule is per-argument rendering, then a single space between arguments —
    // so a rendered list's newlines land inside the one line it was joined into.
    let out = run_with_input("xs = [a b]\nputs head $xs tail\nprint one $xs two\nputs\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "head a\nb tail\none a\nb two\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn puts_renders_a_list_the_same_way_through_a_redirect_and_a_pipe() {
    // Every path that runs a command expands its own words, so all of them have to
    // agree on the rendering.
    let dir = fresh_dir("puts_list_redirect");
    let file = dir.join("out");
    let out = run_with_input(&format!(
        "xs = [a b]\nputs $xs > {}\nputs $xs | cat\n",
        file.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\n");
    assert!(out.stderr.is_empty());
    assert_eq!(
        std::fs::read_to_string(&file).expect("redirected file"),
        "a\nb\n"
    );
}

#[test]
fn a_captured_puts_renders_its_values_too() {
    // `:capture` reaches a builtin as readily as an external, so it is the fourth
    // entry point that has to agree on the rendering.
    let out = run_with_input(
        "xs = [a b]\nr = puts($xs):capture\nputs $r.out:repr\nm = [k: v]\ns = print($m):capture\nputs $s.out:repr\nj = sleep 0.1 &\nt = puts($j):capture\nputs recovered\nwait $j\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("'a\\nb\\n'\n'k: v'\n"), "{stdout:?}");
    assert!(stdout.contains("recovered\n"), "{stdout:?}");
    // The no-byte-form refusal is still an error rather than landing in the record
    // as text.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("puts: a job handle has no text form"),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_styled_value_behaves_as_its_text_everywhere_but_a_renderer() {
    // `DESIGN.md` §"Hooks and the prompt": a styled value is a string carrying
    // display attributes, not a type of its own. Every boundary here wants bytes,
    // and every one of them sees the text — piped, so nothing decorates.
    let out = run_with_input(
        "r = style(\"danger\", fg: red)\n\
         puts $r\n\
         puts \"in a string: $r\"\n\
         puts $r:len $r:upper\n\
         puts $r:repr\n\
         if $r == danger { puts equal }\n\
         if $r < zzz { puts ordered }\n\
         if $r ~ re(dang) { puts matched }\n\
         if $r !~ re(safe) { puts unmatched }\n\
         p = style(\"a:b\", fg: red)\n\
         parts = $p:split(\":\")\n\
         puts $parts:repr\n\
         xs = [danger safe]\n\
         if $r in $xs { puts member }\n\
         x = $r\n\
         x += !\n\
         puts $x:repr\n\
         ys = [$r safe]\n\
         j = $ys:join(\",\")\n\
         puts $j:repr\n\
         /bin/echo argv: $r\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "danger\n\
         in a string: danger\n\
         6 DANGER\n\
         'danger'\n\
         equal\n\
         ordered\n\
         matched\n\
         unmatched\n\
         ['a', 'b']\n\
         member\n\
         'danger!'\n\
         'danger,safe'\n\
         argv: danger\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn style_needs_a_text_argument_and_known_attributes() {
    for (src, needle) in [
        ("r = style()\n", "style() requires one text argument"),
        ("r = style(a, b)\n", "style() takes one text argument"),
        ("r = style(a, fg: chartreuse)\n", "is not a color name"),
        ("r = style(a, colour: red)\n", "no `colour` attribute"),
        ("r = style(a, bold: yes)\n", "`bold` must be a boolean"),
        ("r = style(...[a])\n", "does not accept spread arguments"),
        // The text comes through the same rendering `puts` uses, so a value with no
        // byte form is the same loud error here as there.
        (
            "j = sleep 0.1 &\nr = style($j)\n",
            "a job handle has no text form",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts recovered\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("recovered\n"),
            "{src:?}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn style_with_no_attributes_is_just_the_string() {
    // One representation per meaning: a call that named nothing has nothing to
    // render, so it yields a plain string rather than a styled value that would
    // print identically. `:repr` is how that shows without a terminal.
    let out = run_with_input(
        "a = style(x)\nputs $a:repr\nb = style(5)\nputs $b:repr\nc = style(true)\nputs $c:repr\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "'x'\n'5'\n'true'\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn a_link_is_a_styled_value_and_behaves_as_its_text() {
    // `link` builds the same value `style` does, so everything that made a styled
    // value safe to compute with holds for a hyperlink too. Piped, so the escapes
    // are absent — which is the assertion.
    let out = run_with_input(
        "u = link(docs, \"https://x.test/a?b=c\")\n\
         puts $u\n\
         puts $u:repr\n\
         puts $u:len\n\
         puts \"see $u\"\n\
         if $u == docs { puts equal }\n\
         /bin/echo argv: $u\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "docs\n'docs'\n4\nsee docs\nequal\nargv: docs\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn link_needs_a_text_a_url_and_a_scheme() {
    for (src, needle) in [
        ("u = link()\n", "link() requires a text and a url argument"),
        ("u = link(a)\n", "link() requires a text and a url argument"),
        (
            "u = link(a, b, c)\n",
            "link() takes a text and a url argument",
        ),
        // A terminal needs an absolute URI, so a bare path is a link that silently
        // does nothing — said rather than guessed at `file://`.
        ("u = link(a, \"/etc/passwd\")\n", "has no scheme"),
        ("u = link(a, \"12://x\")\n", "does not start with a scheme"),
        (
            "u = link(a, url: \"https://x.test/\")\n",
            "no `url` argument",
        ),
        ("u = link(...[a b])\n", "does not accept spread arguments"),
        // A collection as the url almost certainly means the arguments were swapped.
        ("u = link(a, [x])\n", "the url must be a string, not a list"),
        (
            "j = sleep 0.1 &\nu = link($j, \"https://x.test/\")\n",
            "a job handle has no text form",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts recovered\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("recovered\n"),
            "{src:?}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn puts_does_not_retype_a_written_argument() {
    // Integer parsing governs value positions, not argument words (`DESIGN.md`
    // §"Literals"), so what was written is what is printed — a leading zero and a
    // sign survive. A *variable* holding `007` really is the integer 7.
    let out = run_with_input("puts 007 -0 +5 1.50\nn = 007\nputs $n\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "007 -0 +5 1.50\n7\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn puts_refuses_a_value_with_no_text_form_and_recovers() {
    for (src, needle) in [
        (
            "j = sleep 0.2 &\nputs $j\n",
            "a job handle has no text form",
        ),
        (
            "xs = [a [b c]]\nputs $xs\n",
            "a list inside a list has no rendering",
        ),
        (
            "m = [k: [a b]]\nputs $m\n",
            "a list inside a map has no rendering",
        ),
    ] {
        let out = run_with_input(&format!("{src}puts recovered\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{src:?}: {stderr:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("recovered\n"),
            "{src:?}: {:?}",
            out.stdout
        );
    }
}

#[test]
fn puts_and_print_still_answer_help() {
    for name in ["puts", "print"] {
        let out = run_with_input(&format!("{name} --help\n"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!("Usage: {name} [ARG ...]")),
            "{stdout:?}"
        );
    }
}

#[test]
fn a_builtin_takes_the_dash_dash_terminator_out_of_the_way() {
    // `DESIGN.md` §"Command resolution and help" offers `--` as the escape from the
    // generated `--help`, and a builtin has to honor it the way a function does —
    // ending the search *and* consuming the terminator. Left in, the escape stopped
    // the detection and then printed the `--` it was written with.
    let out = run_with_input(
        "puts -- --help\n\
         puts -- a b\n\
         puts a -- b\n\
         puts -- -- x\n\
         v = --help\n\
         puts -- $v\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // Only the *first* `--` goes: a literal one stays writable after it.
        "--help\na b\na b\n-- x\n--help\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_builtin_with_options_keeps_the_terminator_for_its_own_parser() {
    // Only the builtin knows where its options end, so it consumes `--` itself.
    // Removing it centrally would undo the thing it was written for.
    //
    // `kill` reads a leading `-SIGNAL`, so after `--` a `-9` is a *target*.
    let out = run_with_input("kill -- -9\nputs status=$sh.status\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("-9") && !stderr.contains("--:"),
        "`-9` should reach kill as a target, not a signal: {stderr:?}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "status=1\n");

    // `disown` reads `-h`/`-a`/`-r`, so after `--` a `-h` is a job reference — and
    // there is no job by that name, so the job it would have marked is untouched.
    let disown = run_with_input("sleep 30 &\ndisown -- -h\nputs status=$sh.status\njobs\n");
    let stdout = String::from_utf8_lossy(&disown.stdout);
    assert!(stdout.contains("status=1"), "{stdout:?}");
    assert!(stdout.contains("Running sleep 30"), "{stdout:?}");
    assert!(
        String::from_utf8_lossy(&disown.stderr).contains("-h: no such job"),
        "{:?}",
        String::from_utf8_lossy(&disown.stderr)
    );

    // `prompt` reads `--reset`, so after `--` it is the prompt text. A prompt is
    // exactly the kind of value that can start with a dash.
    let set = run_with_input("prompt -- --reset\nprompt\n");
    assert_eq!(String::from_utf8_lossy(&set.stdout), "--reset\n");
    assert!(set.stderr.is_empty(), "{:?}", set.stderr);

    // Without the terminator it is still the option it looks like.
    let reset = run_with_input("prompt 'x> '\nprompt --reset\nprompt\n");
    assert_eq!(String::from_utf8_lossy(&reset.stdout), "mesh$ \n");

    // `on` reads `--remove`, so after `--` every word is an operand — which
    // is what lets a hook be *named* `--remove`, the case the terminator exists for.
    for (source, label) in [
        ("on -- preprompt p1 h\n", "plain"),
        ("on -- preprompt --remove h\n", "a hook named --remove"),
        ("on -- preexec p1 h\n", "with an event"),
    ] {
        let out = run_with_input(&format!(
            "func h(c) {{ puts hook }}\n{source}puts status=$sh.status\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "status=0\n",
            "{label}: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // And `--remove` without the terminator still removes.
    let removed = run_with_input(
        "func h(c) { puts hook }\non preprompt p1 h\non --remove preprompt p1\nputs status=$sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&removed.stdout), "status=0\n");

    let missing_event = run_with_input(
        "func h() { puts hook }\non p1 h\nputs register=$sh.status\non --remove p1\nputs remove=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&missing_event.stdout),
        "register=2\nremove=2\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&missing_event.stderr)
            .matches("mesh: on: expected EVENT NAME FUNCTION or --remove EVENT NAME")
            .count(),
        2
    );
}

#[test]
fn a_builtin_and_a_function_read_flags_the_same_way() {
    // The rule is one rule, not one per command kind. A word that *is* `--help`
    // asks for help whether it was written or came out of a variable — mesh's
    // expansion safety is about not splitting or globbing a value, not about
    // laundering a word that is a flag — and `--` is the way to mean it as data.
    let out = run_with_input(
        "func g(...xs) { puts $xs:repr }\n\
         v = --help\n\
         g -- --help\n\
         g -- -- x\n\
         g a -- b\n\
         g -- $v\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "['--help']\n['--', 'x']\n['a', 'b']\n['--help']\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // And a bare `--help` reaches the generated help on both paths.
    for command in ["puts --help\n", "func h(x) { puts $x }\nh --help\n"] {
        let out = run_with_input(command);
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Usage:"),
            "{command:?}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
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

/// A canonical temp root holding `one/proj`, `two/proj`, `two/only`, and
/// `here/mine` — enough to test search order, a miss, and the dot exemption.
fn cdpath_tree(tag: &str) -> PathBuf {
    let root = fresh_dir(tag)
        .canonicalize()
        .expect("canonicalize temp dir");
    for path in ["one/proj", "two/proj", "two/only", "here/mine"] {
        std::fs::create_dir_all(root.join(path)).expect("create tree");
    }
    root
}

#[test]
fn cd_searches_cdpath_and_announces_where_it_landed() {
    // `$env.CDPATH` was already a path-type list — splittable, appendable, and
    // exported — while `cd` ignored it, so setting it configured every shell but
    // this one. A hit through a non-empty entry prints, as POSIX asks, because
    // the destination is not the one the operand appears to name.
    let root = cdpath_tree("cdpath_search");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = [{root}/one, {root}/two]\ncd only\npwd\n",
        root = root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{root}/two/only\n{root}/two/only\n", root = root.display()),
        "the announcement, then pwd"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cd_takes_the_first_cdpath_entry_that_holds_the_name() {
    let root = cdpath_tree("cdpath_order");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = [{root}/two, {root}/one]\ncd proj\npwd\n",
        root = root.display()
    ));
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .ends_with(&format!("{root}/two/proj\n", root = root.display())),
        "the earlier entry wins: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cd_falls_back_to_the_current_directory_and_stays_quiet() {
    // A miss must not break a plain `cd subdir`, and the fallback is not a
    // `CDPATH` hit, so it announces nothing.
    let root = cdpath_tree("cdpath_miss");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = [{root}/one]\ncd mine\npwd\n",
        root = root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{root}/here/mine\n", root = root.display())
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_empty_cdpath_entry_is_the_current_directory_and_is_silent() {
    // The way to say "prefer where I am": a leading empty entry, which is the
    // current directory and therefore announces nothing.
    let root = cdpath_tree("cdpath_empty_entry");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = ['', {root}/one]\nmkdir proj\ncd proj\npwd\n",
        root = root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{root}/here/proj\n", root = root.display())
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_dot_relative_cd_never_searches_cdpath() {
    // The POSIX exemption: `.`, `..`, `./x` and `../x` resolve from where you
    // are, so `cd ../` cannot land in a `CDPATH` entry.
    let root = cdpath_tree("cdpath_dot_exempt");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = [{root}/two]\ncd ./only\npwd\n",
        root = root.display()
    ));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("./only"),
        "the dot-relative form should not find two/only: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{root}/here\n", root = root.display())
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_empty_cd_operand_does_not_jump_to_a_cdpath_entry() {
    // `entry/""` is the entry itself, so searching would turn `cd ""` into a
    // jump to the first entry rather than the error it is.
    let root = cdpath_tree("cdpath_empty_operand");
    let out = run_with_input(&format!(
        "cd {root}/here\n$env.CDPATH = [{root}/two]\ncd ''\npwd\n",
        root = root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{root}/here\n", root = root.display()),
        "an empty operand must not move: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A temp directory with a `sub/` inside it, both canonical — a `cd` reports the
/// physical path, so the expectations have to be physical too (the temp dir sits
/// under a symlink on macOS).
fn cd_hook_dirs(tag: &str) -> (PathBuf, PathBuf) {
    let root = fresh_dir(tag)
        .canonicalize()
        .expect("canonicalize temp dir");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).expect("create sub");
    (root, sub)
}

#[test]
fn cd_hooks_run_on_each_side_of_the_move() {
    // `precd` is still in the old directory and is told where it is going;
    // `postcd` is in the new one and is told where it came from.
    let (root, sub) = cd_hook_dirs("cd_hooks_sides");
    let out = run_with_input(&format!(
        "func leaving(to) {{ puts \"leaving $(pwd) for $to\" }}\n\
         func arrived(from) {{ puts \"arrived $(pwd) from $from\" }}\n\
         on precd trace leaving\n\
         on postcd trace arrived\n\
         cd {}\n\
         cd sub\n",
        root.display()
    ));
    let seen = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        seen.contains(&format!(
            "leaving {} for {}\n",
            root.display(),
            sub.display()
        )),
        "precd should run in the old directory with the target: {seen}"
    );
    assert!(
        seen.contains(&format!(
            "arrived {} from {}\n",
            sub.display(),
            root.display()
        )),
        "postcd should run in the new directory with the previous one: {seen}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cd_hooks_fire_per_move_inside_a_function() {
    // Per move, not per function call: deferring to return would run `precd`
    // somewhere other than the directory it promises to run in.
    let (root, sub) = cd_hook_dirs("cd_hooks_function");
    let out = run_with_input(&format!(
        "func note(to) {{ puts \"-> $to\" }}\n\
         on precd n note\n\
         func visit() {{ cd {}\n cd sub }}\n\
         visit\n\
         pwd\n",
        root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "-> {}\n-> {}\n{}\n",
            root.display(),
            sub.display(),
            sub.display()
        )
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cd_inside_a_hook_does_not_dispatch_the_hooks_again() {
    // A handler may `cd` — but re-dispatching would recurse until the stack ran
    // out, so its own move is silent: one hook line, not two.
    let (root, sub) = cd_hook_dirs("cd_hooks_reentrant");
    let other = root.join("other");
    std::fs::create_dir_all(&other).expect("create other");
    let out = run_with_input(&format!(
        "cd {root}\n\
         func arrived(from) {{ puts \"hook $from\"\n cd {other} }}\n\
         on postcd a arrived\n\
         cd sub\n\
         pwd\n",
        root = root.display(),
        other = other.display(),
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("hook {}\n{}\n", root.display(), other.display()),
        "the handler's own cd must not re-enter the hooks"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = sub;
}

#[test]
fn a_failed_cd_runs_neither_hook() {
    let out = run_with_input(
        "func p(to) { puts \"precd $to\" }\n\
         func q(from) { puts \"postcd $from\" }\n\
         on precd p p\n\
         on postcd q q\n\
         cd /nonexistent-mesh-test-directory\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nonexistent-mesh-test-directory"),
        "the failure is still reported"
    );
}

#[test]
fn a_precd_hook_that_wanders_cannot_redirect_the_move() {
    // The target is resolved to an absolute path *before* `precd` runs, so a
    // handler that changes directory itself cannot make a relative outer `cd`
    // land somewhere else — and `$env.OLDPWD` still names where the move began,
    // so `cd -` comes back to the right place.
    let (root, sub) = cd_hook_dirs("cd_hooks_wander");
    let out = run_with_input(&format!(
        "cd {}\n\
         func wander(to) {{ cd sub }}\n\
         on precd w wander\n\
         cd sub\n\
         pwd\n\
         puts $env.OLDPWD\n",
        root.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n{}\n", sub.display(), root.display())
    );
    let _ = std::fs::remove_dir_all(&root);
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
fn a_lone_star_globs_in_value_position() {
    // A space-delimited `*` lexes as the multiplication operator, so every value
    // slot — a `for` header, an assignment, a parenthesised value — has to read it
    // back as the glob it is (`DESIGN.md` §"Loops"). A `*` with a left operand in
    // front of it is still multiplication.
    let dir = fresh_dir("bare_star_value");
    std::fs::write(dir.join("b.ext"), "").unwrap();
    std::fs::write(dir.join("a.ext"), "").unwrap();
    let out = run_with_input(&format!(
        "cd {}\nfor f in * {{ puts $f }}\nxs = *\nputs $xs\nputs (*)\nputs (4 * 3)\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a.ext\nb.ext\na.ext\nb.ext\na.ext\nb.ext\n12\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
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

/// A directory with one file, one subdirectory, and one hidden entry of each —
/// enough to tell the `files` / `dirs` split and the hidden rule apart.
fn glob_family_dir(tag: &str) -> PathBuf {
    let dir = fresh_dir(tag);
    std::fs::write(dir.join("b.ext"), "").unwrap();
    std::fs::write(dir.join("a.ext"), "").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::create_dir(dir.join(".git")).unwrap();
    std::fs::write(dir.join("sub").join("deep.ext"), "").unwrap();
    dir
}

#[test]
fn dirs_and_files_walk_the_working_directory_by_type() {
    // The `for d in dirs() { … }` walk `DESIGN.md` §"Globbing" opens with: a value
    // call, so it answers with a list the loop iterates rather than with a status.
    let dir = glob_family_dir("glob_family_walk");
    let out = run_with_input(&format!(
        "cd {}\nfor d in dirs() {{ puts \"d:$d\" }}\nfor f in files() {{ puts \"f:$f\" }}\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "d:sub\nf:a.ext\nf:b.ext\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_glob_family_answers_with_a_list_value() {
    // A list, not words: it can be bound, joined, and counted like any other, which
    // is the whole point of the call form over the bare literal.
    let dir = glob_family_dir("glob_family_list");
    let out = run_with_input(&format!(
        "cd {}\nfound = glob(\"*.ext\")\nputs $found:join(\",\")\nputs $found:len\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.ext,b.ext\n2\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glob_expands_a_pattern_a_value_would_not() {
    // The pair the design turns on: `ls $p` passes the pattern verbatim because a
    // value never re-globs, and `glob($p)` is how the same string is expanded on
    // purpose.
    let dir = glob_family_dir("glob_family_runtime");
    let out = run_with_input(&format!(
        "cd {}\np = \"*.ext\"\nputs $p\nputs glob($p):join(\" \")\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "*.ext\na.ext b.ext\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_glob_family_takes_a_directory_and_prefixes_what_it_finds() {
    // Entries come back as paths relative to where the caller stands, so an entry of
    // a named directory is usable without rebuilding the prefix by hand.
    let dir = glob_family_dir("glob_family_named");
    let out = run_with_input(&format!(
        "cd {}\nputs files(sub):join(\" \")\nputs files(\"{}/sub\"):join(\" \")\n",
        dir.display(),
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("sub/deep.ext\n{}/sub/deep.ext\n", dir.display())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_glob_family_skips_hidden_entries_as_a_bare_star_does() {
    // The wrappers are `DIR/*` with a type filter, so they inherit the per-component
    // hidden rule rather than inventing a second one: `.git` is a directory and
    // `.hidden` a file, and neither is reported.
    let dir = glob_family_dir("glob_family_hidden");
    let out = run_with_input(&format!(
        "cd {}\nputs dirs():join(\" \")\nputs files():join(\" \")\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sub\na.ext b.ext\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_glob_family_call_that_matches_nothing_is_the_empty_list() {
    // Globbing's no-match rule, all the way through: a missing directory reads as a
    // pattern that matched nothing, so programmatic use never throws.
    let dir = glob_family_dir("glob_family_empty");
    let out = run_with_input(&format!(
        "cd {}\nputs dirs(nowhere):len\nputs glob(\"*.none\"):len\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n0\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_directory_argument_is_no_directory_rather_than_the_root() {
    // The empty path names nothing, so it takes the missing-directory answer. It
    // must not reach the root: `DIR/*` loses a trailing slash on the way to a
    // pattern, which is the one place `""` and `/` could quietly become the same
    // lookup — and the wrong one is unbounded.
    let out = run_with_input("puts files(\"\"):len\nputs dirs(\"\"):len\nputs dirs(\"/\"):len\n");
    let counts = String::from_utf8_lossy(&out.stdout);
    let counts: Vec<_> = counts.lines().collect();
    assert_eq!(&counts[..2], ["0", "0"]);
    // The root really does have subdirectories, so the two answers are distinct
    // rather than both empty by accident.
    assert_ne!(counts[2], "0", "{counts:?}");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn glob_reports_a_malformed_pattern() {
    // Unlike a bare word, which can still be a filename and so falls back to itself,
    // an explicit `glob()` was asked for a pattern and has nothing else to mean.
    let out = run_with_input("puts glob(\"[\")\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("glob():"),
        "{:?}",
        out.stderr
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn the_glob_family_reports_a_directory_that_is_not_a_string() {
    // `dirs(*.ext)` globs before the call sees it, so the argument arrives as a list
    // — a mistake worth naming rather than treating the first match as the directory.
    let dir = glob_family_dir("glob_family_kind");
    let out = run_with_input(&format!(
        "cd {}\nputs dirs(*.ext)\nputs after\n",
        dir.display()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dirs(): the directory must be a string"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capturing_a_glob_family_call_records_its_value() {
    // `:capture` has to route a built-in *value* name to the call path: sending
    // `glob` to the command path would report a command-not-found for a call that
    // ran fine and returned a list.
    let dir = glob_family_dir("glob_family_capture");
    let out = run_with_input(&format!(
        "cd {}\nr = glob(\"*.ext\"):capture\nputs $r.value:join(\" \")\nputs $r.status\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.ext b.ext\n0\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory holding one of everything the type qualifiers name, so each test
/// can ask for a slice of it: two plain files (one executable, one empty), a
/// directory, and a symlink.
fn qualifier_dir(tag: &str) -> PathBuf {
    let dir = fresh_dir(tag);
    std::fs::write(dir.join("plain.txt"), "text").unwrap();
    std::fs::write(dir.join("empty.txt"), "").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        dir.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::os::unix::fs::symlink("plain.txt", dir.join("link")).unwrap();
    dir
}

#[test]
fn glob_qualifiers_filter_by_type() {
    // `*(d)` is the loop `DESIGN.md` §"Loops" reaches for, and the letters are
    // `find -type`'s. `lstat`, so the symlink is `l` and never `f`.
    let dir = qualifier_dir("glob_qual_type");
    let out = run_with_input(&format!(
        "cd {}\nputs *(d)\nputs *(f)\nputs *(l)\nputs *(type: dir)\nputs *(type: file|dir)\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "sub\nempty.txt plain.txt run.sh\nlink\nsub\nempty.txt plain.txt run.sh sub\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glob_qualifiers_filter_by_the_boolean_tests() {
    // `x` / `exec:` and `empty:` are orthogonal to the type, so they combine with
    // it rather than replacing it — `*(f, x)` is the executable *files*, which is
    // what keeps the directory out of the answer.
    let dir = qualifier_dir("glob_qual_bool");
    let out = run_with_input(&format!(
        "cd {}\nputs *(x)\nputs *(f, x)\nputs *(f, empty: true)\nputs *(exec: false)\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "run.sh sub\nrun.sh\nempty.txt\nempty.txt link plain.txt\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glob_qualifiers_work_in_value_position() {
    // The whole point of the feature is the loop header, so the expression path
    // gets the same treatment as the argument one — and a modifier still chains
    // onto the qualified glob.
    let dir = qualifier_dir("glob_qual_value");
    let out = run_with_input(&format!(
        "cd {}\nfor f in *(d) {{ puts $f }}\nxs = *(f)\nputs $xs:len\nputs (*(d))\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sub\n3\nsub\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_a_bare_glob_takes_qualifiers() {
    // The attached `(` means qualifiers only when the word is a pattern. A call
    // keeps its arguments, and a *quoted* star is not a pattern at all, so neither
    // reading changes.
    let out = run_with_input("puts style(x, fg: red)\nputs (4 * 3)\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x\n12\n");
    let quoted = run_with_input("puts \"*\"(d)\n");
    assert!(
        !String::from_utf8_lossy(&quoted.stderr).contains("glob qualifier"),
        "a quoted star is a string, not a glob: {:?}",
        quoted.stderr
    );
}

#[test]
fn one_dimension_may_be_answered_only_once() {
    // The comma is an `and`, so a second answer to the same question is a
    // contradiction or a silent overwrite. A second *type* is neither, since a path
    // has exactly one — `*(f, d)` can only have meant the alternation, and naming
    // that is better than quietly reading it as `file|dir`.
    for (source, wanted) in [
        ("puts *(f, d)\n", "a glob takes one type"),
        ("puts *(type: file, type: dir)\n", "a glob takes one type"),
        (
            "puts *(exec: true, exec: false)\n",
            "the glob qualifier `exec` is given twice",
        ),
        // `x` and `exec:` are one dimension in two spellings.
        (
            "puts *(x, exec: false)\n",
            "the glob qualifier `exec` is given twice",
        ),
        (
            "puts *(empty: true, empty: false)\n",
            "the glob qualifier `empty` is given twice",
        ),
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(wanted),
            "{source:?} should report {wanted:?}: {:?}",
            out.stderr
        );
    }
}

#[test]
fn only_a_file_or_a_directory_can_be_empty() {
    // A fifo reports a zero length without that saying anything about its contents,
    // as do sockets and most device nodes, so reading the number would sweep every
    // one of them into `*(empty: true)`. `find -empty` draws the same line.
    let dir = fresh_dir("glob_qual_empty_kind");
    std::fs::write(dir.join("empty.txt"), "").unwrap();
    let fifo = dir.join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    assert!(made, "mkfifo is needed to tell the two apart");
    let out = run_with_input(&format!(
        "cd {}\nputs *(empty: true)\nputs *(p)\nputs *(p, empty: true)\nputs *(p, empty: false)\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "empty.txt\npipe\n\npipe\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_glob_qualifier_says_what_it_does_not_recognize() {
    for (source, wanted) in [
        ("puts *(q)\n", "`q` is not a glob qualifier"),
        ("puts *(kind: file)\n", "`kind` is not a glob qualifier"),
        (
            "puts *(type: blue)\n",
            "`blue` is not a value for the glob qualifier `type`",
        ),
        (
            "puts *(exec: maybe)\n",
            "`maybe` is not a value for the glob qualifier `exec`",
        ),
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(wanted),
            "{source:?} should report {wanted:?}: {:?}",
            out.stderr
        );
    }
}

#[test]
fn a_dot_led_pattern_finds_the_dotfiles_and_not_the_directory_itself() {
    // The other half of the rule `glob_star_excludes_dotfiles` pins: a wildcard
    // never matches a leading dot, but a literal `.` in the pattern does. `.` and
    // `..` are excluded whatever the pattern — they are the directory's own
    // entries, never what a loop over `.*` is after.
    let dir = fresh_dir("glob_dotfiles");
    std::fs::write(dir.join("visible.txt"), "").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    std::fs::write(dir.join(".config"), "").unwrap();
    let out = run_with_input(&format!(
        "cd {}\nputs .*\nputs .h*\nputs .[hc]*\nputs ./.*\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ".config .hidden\n.hidden\n.config .hidden\n.config .hidden\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dot_led_pattern_finds_dotfiles_in_a_named_directory() {
    // The same rule one component in, where `.` and `..` are spelled `sub/.` and
    // `sub/..` — a form `Path` normalizes away, so the exclusion cannot lean on it.
    let dir = fresh_dir("glob_dotfiles_sub");
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/.innerdot"), "").unwrap();
    std::fs::write(dir.join("sub/inner.txt"), "").unwrap();
    let out = run_with_input(&format!("cd {}\nputs sub/.*\nputs sub/*\n", dir.display()));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "sub/.innerdot\nsub/inner.txt\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_relative_path_word_parses_in_value_position() {
    // `.` and `..` lex as their own tokens, so a value slot has to stitch the run
    // back into the one word it looks like — `puts ./x` always worked while
    // `x = ./x` was a syntax error.
    let dir = fresh_dir("dot_value");
    std::fs::write(dir.join("a.ext"), "").unwrap();
    std::fs::write(dir.join(".hidden"), "").unwrap();
    let out = run_with_input(&format!(
        "cd {}\nx = ./a.ext\nputs $x\nfor f in ./* {{ puts $f }}\nd = .\nputs $d\nputs ../{}\nu = ../{}\nputs $u\ny = .*\nputs $y\n",
        dir.display(),
        dir.file_name().unwrap().to_string_lossy(),
        dir.file_name().unwrap().to_string_lossy(),
    ));
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("./a.ext\na.ext\n.\n../{name}\n../{name}\n.hidden\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_range_still_wins_the_spellings_it_owns() {
    // The `../x` reading keys off the attached `/`, which no operand can start
    // with, so every range spelling is untouched.
    let out = run_with_input("puts (1..3)\nputs (..3)\nxs = [9 8 7]\nputs $xs[1..]\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n2\n0\n1\n2\n8\n7\n"
    );
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
    // The rule is about the *argv* boundary: an external command needs bytes and
    // there is no canonical separator to pick. `puts` renders instead — see
    // `puts_renders_a_list_one_element_per_line`.
    let out = run_with_input("xs = [a b]\n/bin/echo $xs\nputs recovered\n");
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
        "xs = [a b c d]\nputs ...$xs[1..3]\nputs ...$xs[..=1]\nputs ...$xs[-2..]\nputs ...$xs[..=-1]\nputs ...$xs[..=9223372036854775807]\nputs before ...$xs[9..] after\nputs before ...$xs[..=-5] after\nputs before ...$xs[..=-4] after\n/bin/echo $xs[1..2]\ns = text\nputs $s[1..]\nputs $missing[1..]\nputs recovered\n",
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
fn leading_underscore_is_a_variable_name() {
    // Bare `_` remains reserved, while longer names beginning with an underscore
    // can be bound and read like names with an alphabetic head.
    let out = run_with_input("_private = ok\nputs $_private\nputs \"$_\"\nputs after\n");
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n$_\nafter\n");
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

    await_pty_harness(harness);
}

#[test]
fn new_foreground_job_does_not_receive_sigcont() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(sigcont_harness(&exec)) };
    }
    await_pty_harness(harness);
}

#[test]
fn spawn_failure_returns_terminal_to_interactive_shell() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(spawn_failure_harness(&exec)) };
    }
    await_pty_harness(harness);
}

#[test]
fn a_piped_shell_writes_no_terminal_sequences() {
    // The marks, the working-directory report, the window title and
    // bracketed-paste mode are all for a terminal that is drawing the session. Everything else reading mesh's
    // stdout — a pipe, a file, the several hundred assertions in this file — asked
    // for the command's bytes and nothing else, so any of them reaching those
    // callers would be corruption rather than decoration.
    //
    // What this actually pins is *where they are written from*. Today no piped
    // path reaches `semantic_mark`, `report_cwd` or `set_title` at all: all three
    // are called only from the interactive loop, and their `interactive` check is a second lock on
    // a door that is already shut. So deleting that check does not fail this test —
    // moving a call somewhere shared, like `run_line` or the `cd` builtin, does,
    // which is the mistake actually available to make.
    //
    // `mesh -s` is the case worth pinning among the three: it reads commands from
    // a terminal without being an interactive session, which is why the shell
    // records interactivity rather than deriving it from `isatty`.
    for out in [
        run_with_input("puts hi\nsh -c 'exit 3'\nputs after\n"),
        run_with_args(&["-c", "puts hi\nsh -c 'exit 3'\n"]),
        run_with_args(&["-s"]),
    ] {
        for stream in [&out.stdout, &out.stderr] {
            let text = String::from_utf8_lossy(stream);
            assert!(
                !text.contains("\x1b]133"),
                "a mark escaped to a pipe: {text:?}"
            );
            assert!(!text.contains("133;"), "a mark escaped to a pipe: {text:?}");
            assert!(
                !text.contains("\x1b]7;"),
                "a cwd report escaped to a pipe: {text:?}"
            );
            assert!(
                !text.contains("\x1b[?2004"),
                "bracketed paste escaped to a pipe: {text:?}"
            );
            assert!(
                !text.contains("\x1b]0;") && !text.contains("\x1bk"),
                "a window title escaped to a pipe: {text:?}"
            );
            assert!(
                !text.contains("\x1b]9;"),
                "a notification escaped to a pipe: {text:?}"
            );
        }
    }
}

#[test]
fn an_interactive_shell_marks_where_the_command_ended() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(semantic_mark_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `C` before the command's output and `D` after it, carrying its status.
///
/// The status is what makes `D` worth having over a prompt: a terminal — or a
/// test — reads the outcome from the stream instead of inferring it from which
/// prompt glyph was painted, and a repaint cannot forge it, because the shell
/// writes it once when the command actually ends.
fn semantic_mark_harness(exec: &MeshExec) -> i32 {
    let mut master = -1;
    let mut slave = -1;
    if open_pty_pair(&mut master, &mut slave) != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
    {
        return 40;
    }
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return 41;
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
    if unsafe { libc::setpgid(mesh, mesh) } < 0 && unsafe { libc::getpgid(mesh) } != mesh {
        return 42;
    }
    unsafe { libc::close(slave) };
    if unsafe { libc::tcsetpgrp(master, mesh) } < 0 || !pty_wait_for_prompt(master) {
        return 43;
    }
    // Hooks that print. Their output is produced *because* the command was
    // submitted, so a terminal folding "this command's output" should get it —
    // which means both marks have to sit outside the hooks, not inside them.
    for line in [
        "func pre(c) { puts PREHOOK }\n",
        "func post(c, s, e) { puts POSTHOOK }\n",
        "on preexec p1 pre\n",
        "on postexec p2 post\n",
    ] {
        if unsafe { libc::write(master, line.as_ptr().cast(), line.len()) } != line.len() as isize
            || pty_read_until_command_done(master).is_none()
        {
            return 44;
        }
    }
    // A failing command, so the status in `D` is one no default could produce.
    let command = b"sh -c 'exit 3'\n";
    if unsafe { libc::write(master, command.as_ptr().cast(), command.len()) }
        != command.len() as isize
    {
        return 45;
    }
    let Some((seen, status)) = pty_read_until_command_done(master) else {
        return 46;
    };
    if status != 3 {
        return 47;
    }
    // Both hooks *inside* the region the marks bracket — the claim itself,
    // rather than a chain of positions that happens to encode it.
    //
    // Searching the whole buffer for the first `PREHOOK` does not work: the line
    // editor echoes what is typed, so the literal text is already on the wire
    // from `func pre(c) { puts PREHOOK }` several commands earlier. Whatever of
    // that echo is still unread when this read begins sits ahead of `C`, and a
    // first-occurrence search then reports the hook as *preceding* the mark —
    // intermittently, depending on how much was drained. Slicing to the region
    // first removes the question.
    let at = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|p| p == needle);
    let (Some(open), Some(close)) = (
        at(&seen, b"\x1b]133;C\x1b\\"),
        at(&seen, b"\x1b]133;D;3\x1b\\"),
    ) else {
        return 48;
    };
    if open >= close {
        return 49;
    }
    let region = &seen[open..close];
    if at(region, b"PREHOOK").is_none() || at(region, b"POSTHOOK").is_none() {
        return 50;
    }
    let quit = b"exit\n";
    if unsafe { libc::write(master, quit.as_ptr().cast(), quit.len()) } != quit.len() as isize {
        return 51;
    }
    let mut reaped = 0;
    if unsafe { libc::waitpid(mesh, &mut reaped, 0) } != mesh {
        return 52;
    }
    unsafe { libc::close(master) };
    0
}

/// A mesh session on its own controlling terminal.
struct PtyShell {
    master: RawFd,
    mesh: libc::pid_t,
    /// Everything the shell wrote before its first prompt was ready. The
    /// sequences it emits per *session* rather than per command — the working
    /// directory report, bracketed-paste mode — are only ever here.
    startup: Vec<u8>,
}

/// Start mesh on a fresh pty (in `cwd`, when given) and read up to the first
/// prompt.
///
/// The six harnesses above each open their own pty inline and predate this; they
/// are left as they are rather than mechanically rewritten here.
fn start_pty_shell(exec: &MeshExec, cwd: Option<&Path>) -> Option<PtyShell> {
    start_pty_shell_ready(exec, cwd, INPUT_READY)
}

/// As [`start_pty_shell`], but waiting for `ready` instead of the `OSC 133` `B`.
///
/// A session speaking VS Code's dialect never writes that mark, so "wait for the
/// prompt" has to name which prompt-end it is waiting for.
fn start_pty_shell_ready(exec: &MeshExec, cwd: Option<&Path>, ready: &[u8]) -> Option<PtyShell> {
    // Built before the fork: only async-signal-safe calls are allowed between
    // fork and exec, and allocating a `CString` is not one.
    let directory = cwd.map(|path| {
        std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a cwd without a NUL")
    });
    let mut master = -1;
    let mut slave = -1;
    if open_pty_pair(&mut master, &mut slave) != 0
        || unsafe { libc::setsid() } < 0
        || unsafe { libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0) } < 0
    {
        return None;
    }
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return None;
    }
    if mesh == 0 {
        unsafe {
            libc::setpgid(0, 0);
            if let Some(directory) = &directory
                && libc::chdir(directory.as_ptr()) != 0
            {
                libc::_exit(126);
            }
            libc::dup2(slave, libc::STDIN_FILENO);
            libc::dup2(slave, libc::STDOUT_FILENO);
            libc::dup2(slave, libc::STDERR_FILENO);
            libc::close(master);
            libc::close(slave);
            libc::_exit(exec_mesh(exec));
        }
    }
    // Set the group from both sides of fork so tcsetpgrp cannot race the child.
    if unsafe { libc::setpgid(mesh, mesh) } < 0 && unsafe { libc::getpgid(mesh) } != mesh {
        return None;
    }
    unsafe { libc::close(slave) };
    if unsafe { libc::tcsetpgrp(master, mesh) } < 0 {
        return None;
    }
    let startup = pty_read_until_one_of(master, &[ready])?;
    Some(PtyShell {
        master,
        mesh,
        startup,
    })
}

/// Send `exit 0` and require the shell to leave cleanly, so a harness that ends
/// happily also proves the session survived what it did to it.
fn stop_pty_shell(shell: PtyShell) -> bool {
    if !pty_write(shell.master, b"exit 0\n") {
        return false;
    }
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(shell.mesh, &mut status, 0) } == shell.mesh;
    unsafe { libc::close(shell.master) };
    reaped && libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
}

fn pty_write(master: RawFd, bytes: &[u8]) -> bool {
    let written = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
    written == bytes.len() as isize
}

/// Everything the shell writes from here until end of file — for what it says on
/// its way out, which `pty_read_until_one_of` cannot see: that reader treats the
/// read of 0 bytes at EOF as failure, and at exit the last bytes and the EOF
/// arrive together.
fn pty_read_to_end(master: RawFd) -> Vec<u8> {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if unsafe { libc::poll(&mut ready, 1, QUIET) } <= 0 {
            break;
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            break;
        }
        seen.extend_from_slice(&chunk[..count as usize]);
    }
    seen
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|part| *part == needle)
        .count()
}

/// The payload of the last `OSC 7` in `bytes` — the report for wherever the shell
/// most recently said it was.
fn last_cwd_report(bytes: &[u8]) -> Option<String> {
    const OPEN: &[u8] = b"\x1b]7;";
    let start = bytes.windows(OPEN.len()).rposition(|part| part == OPEN)? + OPEN.len();
    let rest = &bytes[start..];
    let end = rest.windows(2).position(|part| part == b"\x1b\\")?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

/// Percent-encode a path the way `OSC 7` needs it, spelled out again here so the
/// expectation is an independent reading of the rule rather than a call into the
/// code under test.
fn percent_encoded(path: &Path) -> String {
    let mut encoded = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[test]
fn an_interactive_shell_turns_on_bracketed_paste() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(bracketed_paste_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Pasted text is *inserted*, not executed line by line.
fn bracketed_paste_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 60;
    };
    // The mode being set is what this test pins. reedline's guard defaults to
    // off, so without asking for it nothing writes `CSI ?2004 h` and a real
    // terminal never wraps a paste at all — every newline in it arrives as Enter
    // and every line but the last runs before it can be read.
    if occurrences(&shell.startup, b"\x1b[?2004h") == 0 {
        return 61;
    }
    // The behavior it buys, from this side of the pty: two lines wrapped in the
    // paste markers become one buffer, submitted once. This part would pass
    // without the fix — crossterm parses the markers whether or not the mode was
    // set — so it documents the result rather than proving the cause.
    if !pty_write(shell.master, b"\x1b[200~puts one\nputs two\x1b[201~") {
        return 62;
    }
    if !pty_write(shell.master, b"\n") {
        return 63;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 64;
    };
    if status != 0 {
        return 65;
    }
    // Both lines' output before the *first* `D`: run line by line, `puts one`
    // would have ended a command of its own and the read would have stopped there
    // with `two` still unwritten.
    if occurrences(&seen, b"one\r\n") == 0 || occurrences(&seen, b"two\r\n") == 0 {
        return 66;
    }
    // And one command, not two.
    if occurrences(&seen, b"\x1b]133;C\x1b\\") != 1 {
        return 67;
    }
    if !stop_pty_shell(shell) {
        return 68;
    }
    0
}

#[test]
fn a_blank_line_is_not_a_command() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(blank_line_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// A bare Enter submits nothing, so it gets no marks and fires no hooks.
///
/// Both halves of that matter to a terminal: an empty command block is one more
/// thing to page past when jumping between commands, and it would be badged with
/// a status the user never caused.
/// Counted over the whole session rather than asserted per command: the reads
/// hand back every byte they take, in order, so a total over all of them is exact
/// even though where one read stops and the next begins is not. Which is the only
/// way to be sure a mark is *absent* — a window that happened to end early would
/// otherwise report the blank line as unmarked whatever the shell did.
fn blank_line_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 70;
    };
    let mut seen = shell.startup.clone();
    for line in ["func pre(c) { puts PREHOOK }\n", "on preexec p1 pre\n"] {
        if !pty_write(shell.master, line.as_bytes()) {
            return 71;
        }
        let Some((window, _)) = pty_read_until_command_done(shell.master) else {
            return 72;
        };
        seen.extend_from_slice(&window);
    }
    // The blank line. Read to the next prompt rather than to a `D` — the claim is
    // that there is no `D` to wait for. Type-ahead does not survive a submission,
    // so the next command has to wait for this prompt before it is written.
    if !pty_write(shell.master, b"\n") {
        return 73;
    }
    let Some(window) = pty_read_until_one_of(shell.master, &[INPUT_READY]) else {
        return 74;
    };
    seen.extend_from_slice(&window);
    // A real command after it, with a status no default could produce. This is the
    // positive control: without it, a shell that had stopped marking anything at
    // all would pass the counts below.
    if !pty_write(shell.master, b"sh -c 'exit 7'\n") {
        return 75;
    }
    let Some((window, status)) = pty_read_until_command_done(shell.master) else {
        return 76;
    };
    seen.extend_from_slice(&window);
    if status != 7 {
        return 77;
    }
    // Three commands ran — the two that set the hook up, and the one that failed —
    // so three `C` marks. A blank line that marked itself makes it four.
    if occurrences(&seen, b"\x1b]133;C\x1b\\") != 3 {
        return 78;
    }
    // And one `preexec` firing, for that same failing command. `PREHOOK\r\n` rather
    // than `PREHOOK`, because the line editor echoes the `func` that defines it and
    // repaints as it goes: the hook's *output* is the occurrence that ends the line.
    if occurrences(&seen, b"PREHOOK\r\n") != 1 {
        return 79;
    }
    if !stop_pty_shell(shell) {
        return 89;
    }
    0
}

#[test]
fn a_jobdone_hook_fires_where_the_done_notice_prints() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(jobdone_hook_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Submit no-op commands until `marker` has been read, or give up.
///
/// For anything the shell reports *between* commands: the notice and hook for a
/// finished job land at the top of the loop, so reaching them takes one command
/// to trigger the reap and another to read the result back — but only once the
/// job has ended, which no fixed number of round trips can promise. Everything
/// read is appended to `seen`, so a caller can go on counting over the session.
fn pty_settle_until(master: RawFd, seen: &mut Vec<u8>, marker: &[u8]) -> bool {
    for _ in 0..40 {
        if seen.windows(marker.len()).any(|part| part == marker) {
            return true;
        }
        if !pty_write(master, b"puts .\n") {
            return false;
        }
        let Some((window, _)) = pty_read_until_command_done(master) else {
            return false;
        };
        seen.extend_from_slice(&window);
    }
    seen.windows(marker.len()).any(|part| part == marker)
}

/// `jobdone` fires once per finished job, where `[N] Done` is printed.
///
/// A pty, because this is a prompt-lifecycle hook: the notice and the hook both
/// belong to the interactive loop, and a piped script never reaches it. The
/// job's own status is what the hook is for, so it is one no default produces.
fn jobdone_hook_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 110;
    };
    let mut seen = shell.startup.clone();
    for line in [
        "func done(id, cmd, status) { puts JOBDONE=$id/$status }\n",
        "on jobdone j1 done\n",
    ] {
        if !pty_write(shell.master, line.as_bytes()) {
            return 111;
        }
        let Some((window, _)) = pty_read_until_command_done(shell.master) else {
            return 112;
        };
        seen.extend_from_slice(&window);
    }
    // Backgrounded, so the shell notices it finished rather than waiting through
    // it — the reap at the top of the loop is the path under test.
    if !pty_write(shell.master, b"sh -c 'sleep 0.2; exit 6' &\n") {
        return 113;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 114;
    };
    seen.extend_from_slice(&window);
    // Submit until the hook is seen, rather than assuming a fixed number of
    // round trips outlasts the job's sleep. It takes one command for the loop to
    // reap and a second for the notice to be read back, but only once the job
    // has actually ended — and two pty round trips can finish inside 200ms, at
    // which point a fixed count checks for the hook before there is anything to
    // find. Bounded, so a hook that never fires still fails rather than hangs.
    if !pty_settle_until(shell.master, &mut seen, b"JOBDONE=1/6\r\n") {
        return 115;
    }

    // The hook ran, with the job's own id and status — not the shell's. As in
    // `blank_line_harness`, the trailing CRLF is what distinguishes the hook's
    // *output* from the line editor echoing the `func` that defines it.
    if occurrences(&seen, b"JOBDONE=1/6\r\n") != 1 {
        return 119;
    }
    // And the notice the hook accompanies is still printed: the hook is an
    // addition, not a replacement.
    if occurrences(&seen, b"[1] Done (6)") == 0 {
        return 120;
    }

    // A second job, reaped by `jobs` rather than by the top of the loop. `jobs`
    // reaps before it lists, so it is the path that reports this one — and while
    // `reap` handed its result back to whoever called it, that meant the hook
    // fired for jobs the loop noticed and silently not for these.
    if !pty_write(shell.master, b"sh -c 'sleep 0.4; exit 4' &\n") {
        return 122;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 123;
    };
    seen.extend_from_slice(&window);
    // Waited for *here*, with nothing submitted, so the job ends while the shell
    // sits in `read_line` — after that iteration's reap has already run. Waiting
    // with a mesh command instead lets the next top-of-loop reap collect the job
    // first, which is the path this case exists to avoid: an earlier version did
    // exactly that and passed with the fix reverted.
    std::thread::sleep(std::time::Duration::from_millis(1000));
    if !pty_write(shell.master, b"jobs\n") {
        return 126;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 127;
    };
    seen.extend_from_slice(&window);
    // The hook runs at the top of the loop after `jobs` reaped — the point is
    // that it runs at all, not that it beats the listing.
    if !pty_settle_until(shell.master, &mut seen, b"JOBDONE=2/4\r\n") {
        return 128;
    }
    if occurrences(&seen, b"JOBDONE=2/4\r\n") != 1 {
        return 130;
    }

    // And `fg` on a job that has already finished, which notices and reports it
    // by a third path of its own — handing back the status the record carries
    // rather than signaling an empty process group. It printed the notice
    // without the hook until the two were issued from one place.
    if !pty_write(shell.master, b"sh -c 'sleep 0.3; exit 5' &\n") {
        return 131;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 132;
    };
    seen.extend_from_slice(&window);
    // Ends while the shell waits for input, so the top-of-loop reap has already
    // run and `fg` is the first thing to look.
    std::thread::sleep(std::time::Duration::from_millis(1000));
    if !pty_write(shell.master, b"fg\n") {
        return 133;
    }
    let Some((window, status)) = pty_read_until_command_done(shell.master) else {
        return 134;
    };
    seen.extend_from_slice(&window);
    // `fg` hands back the finished job's own status.
    if status != 5 {
        return 135;
    }
    if !pty_settle_until(shell.master, &mut seen, b"JOBDONE=3/5\r\n") {
        return 136;
    }
    if occurrences(&seen, b"JOBDONE=3/5\r\n") != 1 {
        return 137;
    }

    // A `preprompt` handler can report a job itself — it need only run `jobs`,
    // which reaps before it lists. That happens *after* the drain at the top of
    // the loop, so without a second drain the notice is printed above this
    // prompt while its hook waits for the user to submit another line. The two
    // are meant to be one event, so the ordering is the assertion: the hook's
    // output has to land before the next command starts.
    if !pty_write(shell.master, b"func pp() { sleep 0.6; jobs }\n") {
        return 138;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 139;
    };
    seen.extend_from_slice(&window);
    if !pty_write(shell.master, b"on preprompt p pp\n") {
        return 140;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 141;
    };
    seen.extend_from_slice(&window);
    // Shorter than the handler's sleep, so the job ends while it is running and
    // its `jobs` is the first thing to notice.
    if !pty_write(shell.master, b"sh -c 'sleep 0.2; exit 8' &\n") {
        return 142;
    }
    let mut ordering = Vec::new();
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 143;
    };
    ordering.extend_from_slice(&window);
    // One more command. Everything the shell printed between the two `D` marks
    // belongs to the prompt in between — the notice and, if the ordering holds,
    // the hook with it.
    if !pty_write(shell.master, b"puts after\n") {
        return 144;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 145;
    };
    ordering.extend_from_slice(&window);
    let at = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|p| p == needle);
    let (Some(notice), Some(hook)) = (
        at(&ordering, b"[4] Done (8)"),
        at(&ordering, b"JOBDONE=4/8\r\n"),
    ) else {
        seen.extend_from_slice(&ordering);
        return 146;
    };
    // The `C` that opens the *next* command: the hook must be before it, or it
    // ran a whole prompt late.
    let Some(next) = at(&ordering[notice..], b"\x1b]133;C\x1b\\").map(|at| at + notice) else {
        return 147;
    };
    if hook > next {
        return 148;
    }
    seen.extend_from_slice(&ordering);

    // Last, and it doubles as the shutdown: a job reported by the *same command
    // line that exits*. `jobs` reaps it and prints the notice, and the exit
    // leaves before the loop comes round to drain — so the hook has to run as
    // part of going away, not at a next prompt there will never be.
    //
    // The `preprompt` handler registered above is removed first; it sleeps, and
    // leaving it would run before this prompt and reap the job itself, which is
    // the previous case rather than this one.
    if !pty_write(shell.master, b"on --remove preprompt p\n") {
        return 149;
    }
    let Some((window, _)) = pty_read_until_command_done(shell.master) else {
        return 150;
    };
    seen.extend_from_slice(&window);
    // An `exit` handler, to pin the *order*. It stands for the teardown one is
    // for — `DESIGN.md`'s example is closing a job-publish file — so a `jobdone`
    // that arrived after it would be writing to something already closed.
    for line in ["func bye(s) { puts EXITHOOK }\n", "on exit e bye\n"] {
        if !pty_write(shell.master, line.as_bytes()) {
            return 157;
        }
        let Some((window, _)) = pty_read_until_command_done(shell.master) else {
            return 158;
        };
        seen.extend_from_slice(&window);
    }
    // The handler is replaced in place (same event and name) by one that reports
    // a job *itself* — it need only run `jobs`. A drain that took one list would
    // leave whatever its own handlers queued for a later pass, and at shutdown
    // there is no later pass. Two jobs, staggered so the second finishes while
    // the first one's handler is sleeping.
    for line in [
        "func chain(id, cmd, status) { puts JOBDONE=$id/$status; sleep 0.5; jobs }\n",
        "on jobdone j1 chain\n",
        "sh -c 'sleep 0.2; exit 3' &\n",
        "sh -c 'sleep 0.7; exit 4' &\n",
    ] {
        if !pty_write(shell.master, line.as_bytes()) {
            return 151;
        }
        let Some((window, _)) = pty_read_until_command_done(shell.master) else {
            return 152;
        };
        seen.extend_from_slice(&window);
    }
    // Long enough for the first job to end and not the second, and it ends while
    // the shell waits for input, so the drain at the top of the loop has already
    // run and nothing has noticed it yet.
    std::thread::sleep(std::time::Duration::from_millis(400));
    // A bare `exit`, with no `jobs` to do the noticing. `exit` is a builtin and
    // forks nothing, so no wait runs on the way past either: unless the exit
    // path reaps, the shell leaves without the notice or the hook, having
    // watched the job finish and said nothing. The earlier `jobs; exit` form
    // only ever tested the draining half.
    if !pty_write(shell.master, b"exit 0\n") {
        return 153;
    }
    // To EOF: the last bytes and the end of file arrive together, so the readers
    // that stop at a prompt cannot see what the shell says on its way out.
    let parting = pty_read_to_end(shell.master);
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(shell.mesh, &mut status, 0) } == shell.mesh;
    unsafe { libc::close(shell.master) };
    if !reaped || !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        return 154;
    }
    // The notice is the control: if `jobs` did not report the first job here,
    // this case is not exercising the exit path at all and passing would mean
    // nothing. Ids are not pinned — several cases ran before this one — so the
    // statuses are what identify the two jobs.
    if occurrences(&parting, b"Done (3)") == 0 {
        return 155;
    }
    // Both: the one the exiting line reported, and the one its own handler
    // reported while running. The second is the whole point — a single-pass
    // drain reports the first and drops the second.
    if occurrences(&parting, b"/3\r\n") != 1 || occurrences(&parting, b"/4\r\n") != 1 {
        return 156;
    }
    // And both came before the teardown, not after it.
    let at = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|p| p == needle);
    let (Some(first), Some(second), Some(bye)) = (
        at(&parting, b"/3\r\n"),
        at(&parting, b"/4\r\n"),
        at(&parting, b"EXITHOOK\r\n"),
    ) else {
        return 159;
    };
    if first > bye || second > bye {
        return 160;
    }
    0
}

#[test]
fn an_abandoned_line_is_closed_without_a_status() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(abandoned_line_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Ctrl-C on a half-typed line ends the input region reedline opened at `B`.
///
/// Without it the stream leaves that region open and a terminal reads the next
/// prompt, and everything after it, as more of what the user was typing.
fn abandoned_line_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 90;
    };
    if !pty_write(shell.master, b"half-typed line") || !pty_write(shell.master, b"\x03") {
        return 91;
    }
    // `D` with no status: nothing ran, so there is no outcome to report — and a
    // `D;0` here would badge the abandoned line as a command that succeeded.
    // `ST` immediately after `D` is what distinguishes it from `D;<status>`.
    if pty_read_until_one_of(shell.master, &[b"\x1b]133;D\x1b\\"]).is_none() {
        return 92;
    }
    // The line was cancelled, not the session.
    if !pty_write(shell.master, b"puts alive\n") {
        return 93;
    }
    let Some((seen, _)) = pty_read_until_command_done(shell.master) else {
        return 94;
    };
    if occurrences(&seen, b"alive\r\n") == 0 {
        return 95;
    }
    if !stop_pty_shell(shell) {
        return 96;
    }
    0
}

/// Ctrl-C cancels an interactive `gets`, leaving its variable alone.
///
/// An interactive shell ignores SIGINT while a foreground *job* holds the
/// terminal, but `gets` blocks in the shell's own process, where there is no job
/// to receive the keystroke. Left ignored, Ctrl-C did nothing and the **next
/// line typed was swallowed as the read's input** — so the discriminator is what
/// `$x` holds afterwards, not merely that the shell survived.
#[test]
fn ctrl_c_cancels_an_interactive_gets() {
    let config = fresh_dir("gets interrupt");
    let exec = MeshExec::new(&config);
    let harness = unsafe { libc::fork() };
    assert_ne!(harness, -1, "fork the PTY harness");
    if harness == 0 {
        unsafe { libc::_exit(gets_interrupt_harness(&exec)) };
    }
    await_pty_harness(harness);
}

fn gets_interrupt_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 90;
    };
    // Bind first, so the check below distinguishes "left alone" from "never set".
    if !pty_write(shell.master, b"x = kept\n") {
        return 91;
    }
    if pty_read_until_command_done(shell.master).is_none() {
        return 92;
    }
    // The marker is what keeps the keystroke from beating the command to the
    // terminal and cancelling the *line* instead, which would test nothing. It is
    // not evidence that the read has begun, though — only that the command before
    // it ended — so it is the near side of the gap described on
    // `pty_interrupt_until_command_done`, which is why the keystroke repeats.
    if !pty_write(shell.master, b"puts BLOCKING; gets x\n") {
        return 93;
    }
    if !pty_wait_for_marker(shell.master, b"BLOCKING\r\n") {
        return 94;
    }
    // Ctrl-C while the read blocks. The shell must come back to a prompt rather
    // than sit waiting for a line.
    let Some((_, code)) = pty_interrupt_until_command_done(shell.master) else {
        return 95;
    };
    // The status any interrupted foreground command reports.
    if code != 130 {
        return 96;
    }
    // The read was cancelled, so the binding is untouched — and this line is a
    // command, not the input `gets` was waiting for.
    if !pty_write(shell.master, b"puts CHECK-$x\n") {
        return 97;
    }
    let Some((seen, _)) = pty_read_until_command_done(shell.master) else {
        return 98;
    };
    if occurrences(&seen, b"CHECK-kept\r\n") == 0 {
        return 99;
    }
    if !stop_pty_shell(shell) {
        return 100;
    }
    0
}

#[test]
fn an_interactive_shell_reports_where_it_is() {
    // A directory with a space in it, so the encoding is exercised end to end.
    // Canonical, because `getcwd` answers with symlinks resolved — on macOS the
    // temp directory is reached through one.
    let directory = fresh_dir("osc 7 report")
        .canonicalize()
        .expect("canonicalize the temp directory");
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(cwd_report_harness(&exec, &directory)) };
    }
    await_pty_harness(harness);
}

/// `OSC 7` at the first prompt and again after the shell moves.
///
/// The startup report is the half a shell is easiest to leave out: reporting only
/// on `cd` looks right in the terminal you are sitting in and leaves a fresh
/// session — an `ssh`, a new tab — with nothing to split from until the user
/// happens to move.
fn cwd_report_harness(exec: &MeshExec, directory: &Path) -> i32 {
    let Some(shell) = start_pty_shell(exec, Some(directory)) else {
        return 100;
    };
    let Some(report) = last_cwd_report(&shell.startup) else {
        return 101;
    };
    let Some(rest) = report.strip_prefix("file://") else {
        return 102;
    };
    let want = percent_encoded(directory);
    let Some(host) = rest.strip_suffix(&want) else {
        return 103;
    };
    // The host field ends at the path's leading `/`; a hostname that swallowed
    // part of the path would still have matched the suffix above.
    if host.contains('/') {
        return 104;
    }
    // And again when the shell moves, with the new directory. `/` needs no
    // encoding, which is what makes it a clean second reading of the same rule.
    if !pty_write(shell.master, b"cd /\n") {
        return 105;
    }
    let moved = format!("\x1b]7;file://{host}/\x1b\\");
    if pty_read_until_one_of(shell.master, &[moved.as_bytes()]).is_none() {
        return 106;
    }
    if !stop_pty_shell(shell) {
        return 107;
    }
    0
}

#[test]
fn an_interactive_shell_titles_the_window() {
    let directory = fresh_dir("osc title")
        .canonicalize()
        .expect("canonicalize the temp directory");
    // `TERM` decides which sequence is written, and what the test runner inherited
    // is not a fact the test can assume — under `TERM=dumb` the correct answer is
    // to write nothing at all.
    //
    // `HOME` is pinned for the same reason, raised in review: the title shortens a
    // directory under `$HOME` to `~/…`, so a runner started with `HOME=/tmp` would
    // be told to expect an absolute path while the shell correctly wrote an
    // abbreviated one. A separate directory, not an unwritable one, so history has
    // somewhere to go and startup stays quiet. The abbreviating itself has unit
    // tests; what this asserts is the shape of the title.
    let home = fresh_dir("osc title home");
    let exec = MeshExec::with_environment(
        isolated_config_home(),
        &[
            ("TERM", "xterm-256color"),
            ("USER", "tester"),
            ("HOME", home.to_str().expect("a temp path that is UTF-8")),
        ],
    );
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(title_harness(&exec, &directory)) };
    }
    await_pty_harness(harness);
}

/// Reap a forked pty harness and name the phase it stopped at.
///
/// Each harness returns a distinct code per phase, so that code is the whole
/// diagnosis — but `waitpid` reports it inside an encoded wait status, where it
/// is the high byte. Printing the encoded form is how phase 123 reached
/// `TODO.md` as `0x7b00`, a number nothing in the file explains.
fn await_pty_harness(harness: libc::pid_t) {
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(harness, &mut status, 0) }, harness);
    assert!(
        libc::WIFEXITED(status),
        "PTY harness did not exit (wait status {status:#x})"
    );
    let phase = libc::WEXITSTATUS(status);
    assert!(phase == 0, "PTY harness failed at phase {phase}");
}

/// Wait for a path to appear, up to a deadline.
///
/// How a harness synchronizes with a command it cannot hear: with
/// `shell-integration` off there is no `D` to wait for, and reading the echo back
/// instead would re-answer the cursor-position queries in the accumulated buffer,
/// which reedline takes as input. A file the command creates is neither.
fn wait_for_path(path: &Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// The case a setting is actually *for*: an rc file turning a decoration off, so
/// the session never emits it at all. The startup files run after the line editor
/// is built, which is exactly why the settings are shared with it rather than read
/// out of the variable store when it is constructed.
#[test]
fn a_startup_file_can_turn_a_decoration_off_before_the_first_prompt() {
    let home = fresh_dir("options_rc");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("rc.mesh"), "$sh.options.cwd-report = false\n").unwrap();

    let exec = MeshExec::new(&home);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(rc_disabled_decoration_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Where the shell is at the prompt, what it is running while it runs, and back
/// again afterwards.
///
/// The third of those is the one worth a pty: it is not a separate feature but a
/// consequence of the prompt writing its title every time, and a title that stuck
/// on the last command would be the natural bug.
fn title_harness(exec: &MeshExec, directory: &Path) -> i32 {
    let Some(shell) = start_pty_shell(exec, Some(directory)) else {
        return 110;
    };
    let at_prompt = format!("\x1b]0;tester@{}: {}\x07", host_name(), directory.display());
    if occurrences(&shell.startup, at_prompt.as_bytes()) == 0 {
        return 111;
    }
    // A command long enough to run, so its title is on the wire before the
    // prompt's replaces it.
    if !pty_write(shell.master, b"sh -c 'sleep 0.3'\n") {
        return 112;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 113;
    };
    if status != 0 {
        return 114;
    }
    if occurrences(&seen, b"\x1b]0;sh -c 'sleep 0.3'\x07") == 0 {
        return 115;
    }
    // And the directory title comes back, which is what keeps a finished window
    // from claiming it is still busy. It is written immediately after `D`, so the
    // read above has usually taken it already while draining past the mark —
    // looking in what is in hand before waiting for more is the difference between
    // this test and a ten-second timeout.
    if occurrences(&seen, at_prompt.as_bytes()) == 0
        && pty_read_until_one_of(shell.master, &[at_prompt.as_bytes()]).is_none()
    {
        return 116;
    }
    // And the title is given back when the shell leaves. Raised in review: `exit`
    // set the title to `exit` and then returned without reaching another prompt, so
    // the window kept that name after mesh was gone. Asserted as *the last title
    // written*, not merely as present, since being last is the whole claim.
    if !pty_write(shell.master, b"exit 0\n") {
        return 117;
    }
    let farewell = pty_read_to_end(shell.master);
    let last_title = farewell
        .windows(4)
        .rposition(|part| part == b"\x1b]0;")
        .filter(|at| farewell.get(at + 4) == Some(&0x07));
    if last_title.is_none() {
        return 118;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(shell.mesh, &mut status, 0) } != shell.mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 119;
    }
    unsafe { libc::close(shell.master) };
    0
}

#[test]
fn the_title_setting_turns_the_title_off_and_back_on() {
    let exec = MeshExec::with_environment(
        isolated_config_home(),
        &[("TERM", "xterm-256color"), ("USER", "tester")],
    );
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(title_setting_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `$sh.options.osc-title` off means *no* title sequence, and on again means the
/// title comes back — in one session, without restarting the shell.
///
/// The last part is the one that needed thought: the clear on the way out is owed
/// to any title mesh wrote, so turning the setting off does not cancel it. A shell
/// that stops updating the title still has to stop owning it, or the window keeps
/// the name of a command that finished long ago.
fn title_setting_harness(exec: &MeshExec) -> i32 {
    const OSC_TITLE: &[u8] = b"\x1b]0;";
    let Some(shell) = start_pty_shell(exec, None) else {
        return 140;
    };
    // On to begin with, which is what makes the silence below mean something
    // rather than being a session that never titled anything.
    if occurrences(&shell.startup, OSC_TITLE) == 0 {
        return 141;
    }
    if !pty_write(shell.master, b"$sh.options.osc-title = false\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 142;
    }
    // A command slow enough that its title would be on the wire before the
    // prompt's replaced it — so this covers both the running title and the prompt
    // title that follows it.
    if !pty_write(shell.master, b"sh -c 'sleep 0.3'\n") {
        return 143;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 144;
    };
    if status != 0 || occurrences(&seen, OSC_TITLE) != 0 {
        return 145;
    }
    // Back on, and the next command titles again.
    if !pty_write(shell.master, b"$sh.options.osc-title = true\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"sh -c 'sleep 0.3'\n")
    {
        return 146;
    }
    let Some((seen, _)) = pty_read_until_command_done(shell.master) else {
        return 147;
    };
    if occurrences(&seen, b"\x1b]0;sh -c 'sleep 0.3'\x07") == 0 {
        return 148;
    }
    // Off again, then leave: the clear is still written, because this session put
    // a title there while the setting was on.
    if !pty_write(shell.master, b"$sh.options.osc-title = false\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"exit 0\n")
    {
        return 149;
    }
    let farewell = pty_read_to_end(shell.master);
    let cleared = farewell
        .windows(4)
        .rposition(|part| part == OSC_TITLE)
        .is_some_and(|at| farewell.get(at + 4) == Some(&0x07));
    if !cleared {
        return 150;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(shell.mesh, &mut status, 0) } != shell.mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 151;
    }
    unsafe { libc::close(shell.master) };
    0
}

#[test]
fn a_session_that_never_titles_writes_no_title_at_all() {
    let home = fresh_dir("options_title_rc");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("rc.mesh"), "$sh.options.osc-title = false\n").unwrap();

    let exec = MeshExec::with_environment(&home, &[("TERM", "xterm-256color"), ("USER", "tester")]);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(no_title_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Off from the rc file, so the shell never titles anything — **including on the
/// way out**. The exit clear is owed only for a title actually written, and a
/// session that wrote none owes none; clearing anyway would put on the wire the
/// one sequence the setting exists to prevent.
fn no_title_harness(exec: &MeshExec) -> i32 {
    const OSC_TITLE: &[u8] = b"\x1b]0;";
    let Some(shell) = start_pty_shell(exec, None) else {
        return 160;
    };
    if occurrences(&shell.startup, OSC_TITLE) != 0 {
        return 161;
    }
    if !pty_write(shell.master, b"sh -c 'sleep 0.3'\n") {
        return 162;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 163;
    };
    if status != 0 || occurrences(&seen, OSC_TITLE) != 0 {
        return 164;
    }
    if !pty_write(shell.master, b"exit 0\n") {
        return 165;
    }
    let farewell = pty_read_to_end(shell.master);
    if occurrences(&farewell, OSC_TITLE) != 0 {
        return 166;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(shell.mesh, &mut status, 0) } != shell.mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 167;
    }
    unsafe { libc::close(shell.master) };
    0
}

#[test]
fn changing_term_does_not_orphan_the_title() {
    let exec = MeshExec::with_environment(
        isolated_config_home(),
        &[("TERM", "xterm-256color"), ("USER", "tester")],
    );
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(term_change_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `$env.TERM` is read once, so a session cannot end holding a title it can no
/// longer clear.
///
/// Assigning a terminal with no title used to strand one: the command's title went
/// out under the old `TERM`, and every write afterwards — including the clear at
/// exit — asked the new one and stayed silent, leaving the window named after the
/// assignment. Raised in review on #238.
fn term_change_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 130;
    };
    if !pty_write(shell.master, b"$env.TERM = dumb\n") {
        return 131;
    }
    if pty_read_until_command_done(shell.master).is_none() {
        return 132;
    }
    if !pty_write(shell.master, b"exit 0\n") {
        return 133;
    }
    let farewell = pty_read_to_end(shell.master);
    // Still the empty title last, under the `TERM` the session started with.
    let last_title = farewell
        .windows(4)
        .rposition(|part| part == b"\x1b]0;")
        .filter(|at| farewell.get(at + 4) == Some(&0x07));
    if last_title.is_none() {
        return 134;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(shell.mesh, &mut status, 0) } != shell.mesh
        || !libc::WIFEXITED(status)
        || libc::WEXITSTATUS(status) != 0
    {
        return 135;
    }
    unsafe { libc::close(shell.master) };
    0
}

#[test]
fn clip_copies_through_the_terminal() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(clip_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `clip` reaches the terminal, from an argument and from a pipe.
///
/// A pty is the only place this shows: the sequence goes to `/dev/tty` rather than
/// stdout, precisely so that a redirect or a pipeline cannot swallow it, which also
/// means no piped test can see it arrive.
fn clip_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 140;
    };
    if !pty_write(shell.master, b"clip hello world\n") {
        return 141;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 142;
    };
    if status != 0 {
        return 143;
    }
    // `aGVsbG8gd29ybGQ=` is `hello world`: arguments join with a space, as `puts`
    // does, and nothing is added to what was asked for.
    if occurrences(&seen, b"\x1b]52;c;aGVsbG8gd29ybGQ=\x1b\\") == 0 {
        return 144;
    }
    // From a pipe, with no arguments — and the newline `puts` wrote is part of what
    // was handed over, so `cGlwZWQK` decodes to `piped\n`. `clip` copies what it is
    // given rather than deciding which bytes the user meant.
    if !pty_write(shell.master, b"puts piped | clip\n") {
        return 145;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 146;
    };
    if status != 0 {
        return 147;
    }
    if occurrences(&seen, b"\x1b]52;c;cGlwZWQK\x1b\\") == 0 {
        return 148;
    }
    if !stop_pty_shell(shell) {
        return 149;
    }
    0
}

#[test]
fn vs_code_gets_its_own_dialect_and_the_command_line() {
    let exec = MeshExec::with_environment(
        isolated_config_home(),
        &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "vscode")],
    );
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(vscode_dialect_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// Under `TERM_PROGRAM=vscode` the marks are `OSC 633`, they carry the command
/// line, and no `OSC 133` goes out beside them.
///
/// The last part is the one worth a pty: VS Code parses both dialects, so sending
/// both would have it count every command twice — and that is invisible in a unit
/// test of either sequence on its own.
fn vscode_dialect_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell_ready(exec, None, b"\x1b]633;B\x1b\\") else {
        return 170;
    };
    // A semicolon in the command, since that is what `E` has to escape to survive.
    if !pty_write(shell.master, b"sh -c 'exit 3; puts unreached'\n") {
        return 171;
    }
    let Some(seen) = pty_read_until_one_of(shell.master, &[b"\x1b]633;D;3\x1b\\"]) else {
        return 172;
    };
    if occurrences(&seen, b"\x1b]633;C\x1b\\") == 0 {
        return 173;
    }
    // `E` carries what was typed, with the `;` as `\x3b` so it cannot end the
    // sequence, and it arrives before the output it describes.
    let command_line = b"\x1b]633;E;sh -c 'exit 3\\x3b puts unreached'\x1b\\";
    let at = |needle: &[u8]| seen.windows(needle.len()).position(|part| part == needle);
    let (Some(described), Some(output_start)) = (at(command_line), at(b"\x1b]633;C\x1b\\")) else {
        return 174;
    };
    if described >= output_start {
        return 175;
    }
    // One dialect, not both.
    if occurrences(&seen, b"\x1b]133;") != 0 {
        return 176;
    }
    if !stop_pty_shell(shell) {
        return 177;
    }
    0
}

#[test]
fn a_startup_file_can_choose_the_dialect() {
    // Raised in review on #247: the dialect was snapshotted while the editor was
    // built, which is *before* the startup files run, so an `rc.mesh` setting
    // `$env.TERM_PROGRAM` was read too late to matter. `$env.TERM` was already
    // honored from there, and two neighbouring features reading the environment at
    // different moments is the kind of difference nobody remembers.
    let home = fresh_dir("vscode_rc");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("rc.mesh"), "$env.TERM_PROGRAM = vscode\n").unwrap();

    // Nothing in the environment says VS Code; only the startup file does.
    let exec = MeshExec::with_environment(&home, &[("TERM", "xterm-256color")]);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(startup_dialect_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// The very first prompt already speaks the dialect the startup file asked for.
fn startup_dialect_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell_ready(exec, None, b"\x1b]633;B\x1b\\") else {
        return 180;
    };
    // The first prompt, before any command: `A` and `B` are reedline's, and they are
    // the half that was deciding too early.
    if occurrences(&shell.startup, b"\x1b]633;A;k=i\x1b\\") == 0 {
        return 181;
    }
    if occurrences(&shell.startup, b"\x1b]133;") != 0 {
        return 182;
    }
    if !pty_write(shell.master, b"puts hi\n") {
        return 183;
    }
    let Some(seen) = pty_read_until_one_of(shell.master, &[b"\x1b]633;D;0\x1b\\"]) else {
        return 184;
    };
    if occurrences(&seen, b"\x1b]633;E;puts hi\x1b\\") == 0 {
        return 185;
    }
    if !stop_pty_shell(shell) {
        return 186;
    }
    0
}

#[test]
fn notify_reaches_the_terminal_and_a_quick_command_does_not() {
    let exec = MeshExec::with_environment(isolated_config_home(), &[("TERM", "xterm-256color")]);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(notify_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `notify` reaches the terminal, and a command that finishes quickly raises
/// nothing by itself.
///
/// The automatic notification's *threshold* is unit-tested against
/// `command_notification`, which takes it as an argument — an end-to-end test of
/// the ten-second case would cost ten seconds of suite time to cover one call
/// site. What is worth an end-to-end test is the pair either side of it: that the
/// sequence really reaches a terminal (via the builtin, which writes the same
/// `OSC 9`), and that an ordinary fast command stays silent, which is the failure
/// that would make mesh unusable rather than merely quiet.
fn notify_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 150;
    };
    if !pty_write(shell.master, b"notify hello from mesh\n") {
        return 151;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 152;
    };
    if status != 0 {
        return 153;
    }
    if occurrences(&seen, b"\x1b]9;hello from mesh\x07") == 0 {
        return 154;
    }
    // A command well under the threshold: no notification of its own. The `notify`
    // above proves the channel works, so silence here is a decision rather than a
    // broken pipe.
    if !pty_write(shell.master, b"sh -c 'sleep 0.2'\n") {
        return 155;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 156;
    };
    if status != 0 {
        return 157;
    }
    if occurrences(&seen, b"\x1b]9;") != 0 {
        return 158;
    }
    // The setting reaches the automatic notification: with it off, a command over
    // the threshold would raise nothing. The threshold itself is unit-tested, so
    // what this pins is that the flag is consulted at all — `notify` keeps working,
    // since a builtin is called rather than drawn and needs no off switch.
    if !pty_write(shell.master, b"$sh.options.command-notify = false\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 159;
    }
    if !pty_write(shell.master, b"notify still explicit\n") {
        return 160;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 161;
    };
    if status != 0 || occurrences(&seen, b"\x1b]9;still explicit\x07") == 0 {
        return 162;
    }
    if !stop_pty_shell(shell) {
        return 163;
    }
    0
}

#[test]
fn a_styled_value_colors_only_what_reaches_the_terminal() {
    let exec = MeshExec::with_environment(isolated_config_home(), &[("TERM", "xterm-256color")]);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(style_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// A styled value emits its attributes only where they can be seen.
///
/// A pty is the one place that is visible: every piped test sees stdout that is not
/// a terminal, which is exactly the case where the escapes are *supposed* to be
/// absent — so a piped test can prove the stripping and nothing else.
///
/// The decision is made per command, before its redirections are opened, so what
/// this pins is that each caller answered for its own stdout rather than for the
/// shell's.
fn style_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 200;
    };
    if !pty_write(shell.master, b"r = style(\"danger\", fg: red)\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 201;
    }
    // Bold and a background too, so the parameter order is pinned rather than just
    // the fact that something was emitted.
    if !pty_write(shell.master, b"b = style(hi, fg: blue, bold: true)\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 202;
    }
    if !pty_write(shell.master, b"puts $r\n") {
        return 203;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 204;
    };
    if status != 0 || occurrences(&seen, b"\x1b[31mdanger\x1b[0m") == 0 {
        return 205;
    }
    if !pty_write(shell.master, b"puts $b\n") {
        return 206;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 207;
    };
    // Bold first, then the color: one `SGR` carrying both, not two sequences.
    if status != 0 || occurrences(&seen, b"\x1b[1;34mhi\x1b[0m") == 0 {
        return 208;
    }
    // Re-styling *adds*: the red survives while bold is turned on.
    if !pty_write(shell.master, b"e = style($r, bold: true)\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 209;
    }
    if !pty_write(shell.master, b"puts $e\n") {
        return 210;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 211;
    };
    if status != 0 || occurrences(&seen, b"\x1b[1;31mdanger\x1b[0m") == 0 {
        return 212;
    }
    // A pipe means this stage's stdout is not the terminal, so the text goes
    // through plain even though the shell's own stdout is one.
    if !pty_write(shell.master, b"puts $r | cat\n") {
        return 213;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 214;
    };
    if status != 0 || occurrences(&seen, b"\x1b[31m") != 0 {
        return 215;
    }
    // A redirection is the case the per-command decision exists for: the words are
    // rendered *before* the target is opened, so only asking "does this command
    // retarget stdout" keeps the escapes out of the file.
    let file = fresh_dir("style_redirect").join("out");
    let line = format!("puts $r > {}\n", file.display());
    if !pty_write(shell.master, line.as_bytes())
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 216;
    }
    match std::fs::read(&file) {
        Ok(written) if written == b"danger\n" => {}
        _ => return 217,
    }
    // `NO_COLOR` drops the attributes and keeps the text, with no other change to
    // what the shell writes.
    if !pty_write(shell.master, b"$env.NO_COLOR = 1\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 218;
    }
    if !pty_write(shell.master, b"puts $r\n") {
        return 219;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 220;
    };
    if status != 0 || occurrences(&seen, b"\x1b[31m") != 0 || occurrences(&seen, b"danger") == 0 {
        return 221;
    }
    // `TERM=dumb` is the one terminal name for which SGR is text rather than
    // styling, so it is refused separately from the tty test.
    if !pty_write(shell.master, b"$env.NO_COLOR = ''\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"$env.TERM = dumb\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 222;
    }
    if !pty_write(shell.master, b"puts $r\n") {
        return 223;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 224;
    };
    if status != 0 || occurrences(&seen, b"\x1b[31m") != 0 || occurrences(&seen, b"danger") == 0 {
        return 225;
    }
    if !stop_pty_shell(shell) {
        return 226;
    }
    0
}

#[test]
fn a_link_reaches_a_terminal_that_parses_osc() {
    let exec = MeshExec::with_environment(isolated_config_home(), &[("TERM", "xterm-256color")]);
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(link_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// `OSC 8` on a pty, and the three ways it drops — which are *not* the three ways
/// color drops, and that difference is the point of the test.
fn link_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 240;
    };
    if !pty_write(shell.master, b"u = link(docs, \"https://x.test/a?b=c\")\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 241;
    }
    if !pty_write(shell.master, b"puts $u\n") {
        return 242;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 243;
    };
    // Printable ASCII survives, so `?` and `=` keep the structure that was written.
    // It closes with the empty URI, which is how `OSC 8` says the link ends.
    if status != 0
        || occurrences(
            &seen,
            b"\x1b]8;;https://x.test/a?b=c\x1b\\docs\x1b]8;;\x1b\\",
        ) == 0
    {
        return 244;
    }
    // Composes with `style` in either order — both set the attributes they name on
    // the same value — and the link wraps outside the color so the whole run is
    // clickable.
    for line in [
        b"a = link(style(x, fg: blue), \"https://y.test/\")\n".as_slice(),
        b"b = style(link(x, \"https://y.test/\"), fg: blue)\n".as_slice(),
    ] {
        if !pty_write(shell.master, line) || pty_read_until_command_done(shell.master).is_none() {
            return 245;
        }
    }
    for name in [b"puts $a\n".as_slice(), b"puts $b\n".as_slice()] {
        if !pty_write(shell.master, name) {
            return 246;
        }
        let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
            return 247;
        };
        if status != 0
            || occurrences(
                &seen,
                b"\x1b]8;;https://y.test/\x1b\\\x1b[34mx\x1b[0m\x1b]8;;\x1b\\",
            ) == 0
        {
            return 248;
        }
    }
    // An `ESC` in the URL is percent-encoded rather than ending mesh's own sequence
    // and leaving the payload on screen — the title guard, in URL form.
    if !pty_write(
        shell.master,
        b"e = link(a, \"https://x.test/\\e]0;pwned\\a\")\n",
    ) || pty_read_until_command_done(shell.master).is_none()
    {
        return 249;
    }
    if !pty_write(shell.master, b"puts $e\n") {
        return 250;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 251;
    };
    if status != 0 || occurrences(&seen, b"%1B]0;pwned%07") == 0 {
        return 252;
    }
    if occurrences(&seen, b"\x1b]0;pwned\x07") != 0 {
        return 253;
    }
    // **`NO_COLOR` keeps the link and drops the color.** A hyperlink is not color,
    // and dropping it would lose the URL rather than make the output plainer. This
    // is the one place the two bits visibly disagree.
    if !pty_write(shell.master, b"$env.NO_COLOR = 1\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 254;
    }
    if !pty_write(shell.master, b"puts $a\n") {
        return 255;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 100;
    };
    if status != 0
        || occurrences(&seen, b"\x1b]8;;https://y.test/\x1b\\x\x1b]8;;\x1b\\") == 0
        || occurrences(&seen, b"\x1b[34m") != 0
    {
        return 101;
    }
    // A pipe takes neither, since the stage's stdout is not a terminal at all.
    if !pty_write(shell.master, b"$env.NO_COLOR = ''\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"puts $u | cat\n")
    {
        return 102;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 103;
    };
    if status != 0 || occurrences(&seen, b"\x1b]8;;") != 0 || occurrences(&seen, b"docs") == 0 {
        return 104;
    }
    // `TERM=linux` is why links need the `OSC` allowlist and color does not: it
    // reads `ESC ]` as the start of a palette sequence and would leave the URL on
    // screen. The color still goes out — SGR it does parse.
    if !pty_write(shell.master, b"$env.TERM = linux\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"puts $a\n")
    {
        return 105;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 106;
    };
    if status != 0 || occurrences(&seen, b"\x1b]8;;") != 0 || occurrences(&seen, b"\x1b[34m") == 0 {
        return 107;
    }
    if !stop_pty_shell(shell) {
        return 108;
    }
    0
}

/// This host's name, the way the shell reads it — the title carries it, and
/// hardcoding one would only test the machine the suite happens to run on.
fn host_name() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: `gethostname` writes at most `buffer.len()` bytes through a pointer
    // valid and writable for exactly that many.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return String::new();
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

fn rc_disabled_decoration_harness(exec: &MeshExec) -> i32 {
    let Some(shell) = start_pty_shell(exec, None) else {
        return 130;
    };
    // The first prompt is the startup report — the half `DESIGN.md` asks for and
    // the one an rc file has the least time to prevent. It is not there.
    if occurrences(&shell.startup, b"\x1b]7;") != 0 {
        return 131;
    }
    // Nor after a move, and the shell is otherwise a working session: the marks
    // are untouched, which is what lets this read wait on one.
    if !pty_write(shell.master, b"cd /\n") {
        return 132;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 133;
    };
    if status != 0 || occurrences(&seen, b"\x1b]7;") != 0 {
        return 134;
    }
    if !stop_pty_shell(shell) {
        return 135;
    }
    0
}

#[test]
fn settings_turn_the_interactive_decorations_off_and_back_on() {
    let directory = fresh_dir("options_decorations");
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(decoration_settings_harness(&exec, &directory)) };
    }
    await_pty_harness(harness);
}

/// `$sh.options.cwd-report` and `$sh.options.shell-integration` govern sequences
/// the shell writes, so the proof is the wire: turn one off and the escape stops
/// arriving, turn it back on and it resumes — in the same session, without
/// restarting the shell.
fn decoration_settings_harness(exec: &MeshExec, directory: &Path) -> i32 {
    let Some(shell) = start_pty_shell(exec, Some(directory)) else {
        return 110;
    };
    // The report is on to begin with, which is what makes the silence below mean
    // something rather than just being a session that never reported.
    if last_cwd_report(&shell.startup).is_none() {
        return 111;
    }
    if !pty_write(shell.master, b"$sh.options.cwd-report = false\n")
        || pty_read_until_command_done(shell.master).is_none()
    {
        return 112;
    }
    // Two moves, so the quiet window covers two prompts rather than one: `OSC 7`
    // is written per prompt, and a single prompt's worth could be a read that
    // stopped early rather than a report that never came.
    for line in [b"cd /\n".as_slice(), b"cd /tmp\n".as_slice()] {
        if !pty_write(shell.master, line) {
            return 113;
        }
        let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
            return 114;
        };
        if status != 0 || occurrences(&seen, b"\x1b]7;") != 0 {
            return 115;
        }
    }
    // Back on, and the next move reports again.
    if !pty_write(shell.master, b"$sh.options.cwd-report = true\n")
        || pty_read_until_command_done(shell.master).is_none()
        || !pty_write(shell.master, b"cd /\n")
    {
        return 116;
    }
    let Some((seen, _)) = pty_read_until_command_done(shell.master) else {
        return 117;
    };
    if occurrences(&seen, b"\x1b]7;") == 0 {
        return 118;
    }

    // The marks. Turning them off is itself still marked: the decision is taken
    // once per command, before it runs, so `C` cannot open a region that `D` then
    // declines to close.
    if !pty_write(shell.master, b"$sh.options.shell-integration = false\n") {
        return 119;
    }
    let Some((seen, _)) = pty_read_until_command_done(shell.master) else {
        return 120;
    };
    if occurrences(&seen, b"\x1b]133;D;") != 1 {
        return 121;
    }
    // Two commands with the marks off. Neither can be waited for on the wire, so
    // each announces itself with a file instead; the pty is left unread until the
    // end, which is what puts both commands' bytes in the buffer examined below.
    let quiet = directory.join("quiet");
    let restored = directory.join("restored");
    let mut line = Vec::from(b"touch ");
    line.extend_from_slice(quiet.as_os_str().as_bytes());
    line.push(b'\n');
    if !pty_write(shell.master, &line) || !wait_for_path(&quiet) {
        return 122;
    }
    // Re-enabling and a command in one line, so the harness can hear that the
    // *assignment* landed. It runs with the marks still off, like the `touch`
    // above: the decision for this line was taken before it ran.
    let mut line = Vec::from(b"$sh.options.shell-integration = true ; touch ");
    line.extend_from_slice(restored.as_os_str().as_bytes());
    line.push(b'\n');
    if !pty_write(shell.master, &line) || !wait_for_path(&restored) {
        return 123;
    }
    // Now read everything those two commands wrote, plus one marked command after
    // them. Exactly one `C` and one `D` in the lot: the pair belongs to `puts
    // back`, so the two that ran with the setting off contributed none.
    if !pty_write(shell.master, b"puts back\n") {
        return 124;
    }
    let Some((seen, status)) = pty_read_until_command_done(shell.master) else {
        return 125;
    };
    if status != 0 || occurrences(&seen, b"back\r\n") == 0 {
        return 126;
    }
    if occurrences(&seen, b"\x1b]133;C\x1b\\") != 1 || occurrences(&seen, b"\x1b]133;D;") != 1 {
        return 127;
    }
    if !stop_pty_shell(shell) {
        return 128;
    }
    0
}

fn spawn_failure_harness(exec: &MeshExec) -> i32 {
    let mut master = -1;
    let mut slave = -1;
    if open_pty_pair(&mut master, &mut slave) != 0
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
        || pty_read_until_command_done(master).is_none()
    {
        return 33;
    }
    let command = b"puts recovered\n";
    if unsafe { libc::write(master, command.as_ptr().cast(), command.len()) }
        != command.len() as isize
    {
        return 34;
    }
    let Some((output, _)) = pty_read_until_command_done(master) else {
        return 35;
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
    if open_pty_pair(&mut master, &mut slave) != 0
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
    let Some((output, _)) = pty_read_until_command_done(master) else {
        return 24;
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

/// One Ctrl-C abandons the wait and nothing else, whether the wait named one job
/// or several.
///
/// Both spellings share a session deliberately. A pty session is the most
/// expensive thing in this suite — a second one running beside the first was
/// enough to make unrelated timing-sensitive tests fail about one run in seven,
/// measured against the same suite with this test skipped. Sharing also tests
/// more than two separate sessions would: the second wait starts from the table
/// the first interrupt left behind, which is the very claim being made.
#[test]
fn an_interrupt_abandons_a_wait_and_leaves_the_jobs_alone() {
    let exec = MeshExec::new(isolated_config_home());
    let harness = unsafe { libc::fork() };
    assert!(harness >= 0);
    if harness == 0 {
        unsafe { libc::_exit(wait_interrupt_harness(&exec)) };
    }
    await_pty_harness(harness);
}

/// A wait blocks on a job that does *not* hold the terminal, so the SIGINT a
/// Ctrl-C generates reaches the shell rather than the job. The shell ignores
/// SIGINT at the prompt, which would leave nothing to end the wait: without a
/// catcher installed around it this hangs until the job finishes on its own.
///
/// Two rounds in one session — `wait 1`, then `wait 1 2` — since a second pty
/// session costs more than the second round does. Every step returns its own
/// code so a failure says which one gave up.
///
/// The signal is delivered directly rather than typed. What is under test is
/// what mesh does with a SIGINT during a wait, and driving that through the line
/// discipline instead only adds a race against reedline's raw mode, in which the
/// same keystroke means "cancel this line" rather than "interrupt".
fn wait_interrupt_harness(exec: &MeshExec) -> i32 {
    let mut master = -1;
    let mut slave = -1;
    if open_pty_pair(&mut master, &mut slave) != 0 {
        return 40;
    }
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let mesh = unsafe { libc::fork() };
    if mesh < 0 {
        return 41;
    }
    // mesh takes the session and the terminal itself, rather than being placed
    // in one by the harness: the point of the test is a signal the *terminal*
    // generates, so mesh has to be the foreground group of its own controlling
    // terminal exactly as it is under a real one.
    if mesh == 0 {
        unsafe {
            libc::setsid();
            libc::ioctl(slave, mesh_platform::TIOCSCTTY, 0);
            libc::dup2(slave, libc::STDIN_FILENO);
            libc::dup2(slave, libc::STDOUT_FILENO);
            libc::dup2(slave, libc::STDERR_FILENO);
            libc::close(master);
            libc::close(slave);
        }
        unsafe { libc::_exit(exec_mesh(exec)) };
    }
    unsafe { libc::close(slave) };
    if !pty_wait_for_prompt(master) {
        return 43;
    }

    // Two jobs, so the same session can ask about one operand and about several.
    // Neither ever finishes on its own within the test, so a wait that is not
    // interrupted blocks until the harness gives up — which is the failure this
    // is looking for.
    for job in ["sleep 30 &", "sleep 31 &"] {
        if unsafe { libc::write(master, job.as_ptr().cast(), job.len()) } != job.len() as isize
            || unsafe { libc::write(master, b"\n".as_ptr().cast(), 1) } != 1
            || pty_read_until_command_done(master).is_none()
        {
            return 44;
        }
    }

    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    // `wait 1` first, then `wait 1 2` against the table the first interrupt left
    // behind. The second is the case that used to hang: the interruption came
    // back as an ordinary 130, so the operand loop read it as job 1 merely
    // failing and blocked on job 2, and the prompt never returned.
    for waiting in ["wait 1\n", "wait 1 2\n"] {
        if unsafe { libc::write(master, waiting.as_ptr().cast(), waiting.len()) }
            != waiting.len() as isize
        {
            return 45;
        }

        let mut seen = Vec::new();
        // Let the line reach the shell before interrupting anything. Until
        // reedline hands the terminal back, Ctrl-C is a keystroke that cancels
        // the line rather than a signal, so an eager one would interrupt nothing
        // and leave the wait unstarted. The repaint stops once the line is
        // submitted, so silence is the evidence that it has been. This also
        // drains whatever the previous round left unread.
        loop {
            if unsafe { libc::poll(&mut ready, 1, 400) } <= 0 {
                break;
            }
            let mut chunk = [0_u8; 256];
            let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count <= 0 {
                return 53;
            }
            let fresh = seen.len().saturating_sub(3);
            seen.extend_from_slice(&chunk[..count as usize]);
            // Answer only queries in the bytes that just arrived. Scanning the
            // whole buffer re-answers every earlier query on every read, and
            // reedline takes the replies as *input* — the line stops being empty,
            // so Ctrl-D no longer exits and a harness waiting on the shell waits
            // forever. The three-byte overlap is for a query split across two
            // reads.
            if seen[fresh..].windows(4).any(|part| part == b"\x1b[6n") {
                unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
            }
        }
        seen.clear();

        // One SIGINT, delivered the way the terminal would deliver it. Nothing
        // is printed on entering the wait, so the *failed* prompt is the evidence
        // that the wait came back non-zero — `mesh$` would be repainted on any
        // keystroke, wait or no wait.
        if unsafe { libc::kill(mesh, libc::SIGINT) } != 0 {
            return 46;
        }
        let mut abandoned = false;
        for _ in 0..20 {
            if unsafe { libc::poll(&mut ready, 1, 500) } > 0 {
                let mut chunk = [0_u8; 256];
                let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
                if count <= 0 {
                    return 47;
                }
                seen.extend_from_slice(&chunk[..count as usize]);
                if seen.windows(4).any(|part| part == b"\x1b[6n") {
                    unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
                }
            }
            if seen.windows(5).any(|part| part == b"mesh!") {
                abandoned = true;
                break;
            }
        }
        if !abandoned {
            return 48;
        }

        // The wait reports the interruption, and the wait is all that was
        // abandoned: both jobs are still running and still listed, so nothing was
        // taken out from under a later `fg`.
        let probe = "puts s=$sh.status\n";
        if unsafe { libc::write(master, probe.as_ptr().cast(), probe.len()) }
            != probe.len() as isize
            || !pty_wait_for_marker(master, b"s=130")
        {
            return 49;
        }
        // One listing, both markers. Asking twice would leave the first
        // listing's tail unread — the second wait matches those leftover bytes
        // and returns before the second listing has even been read — and an
        // unread listing backs the pty up until the shell blocks writing into
        // it, which presents as the next line typed being ignored and the
        // harness waiting on a shell that will never leave.
        let listing = "jobs\n";
        if unsafe { libc::write(master, listing.as_ptr().cast(), listing.len()) }
            != listing.len() as isize
            || !pty_wait_for_markers(master, &["Running sleep 30", "Running sleep 31"])
        {
            return 52;
        }
    }

    if unsafe { libc::write(master, b"exit\n".as_ptr().cast(), 5) } != 5 {
        return 50;
    }
    let mut status = 0;
    if unsafe { libc::waitpid(mesh, &mut status, 0) } != mesh || !libc::WIFEXITED(status) {
        return 51;
    }
    unsafe { libc::close(master) };
    0
}

fn background_startup_harness(exec: &MeshExec) -> i32 {
    use std::os::fd::RawFd;

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    if open_pty_pair(&mut master, &mut slave) != 0
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

    // A descriptor above the standard three is the same leak by another route:
    // `3>&1` gives the stage a second handle on the pipe, and installing it left
    // the original open for the life of a stage that never `exec`s, so the
    // nested command inherited it however thoroughly it redirected its own.
    let (high, elapsed) = timed(
        "func f() { sleep 5 > /dev/null 2> /dev/null 3> /dev/null &\nputs hi }\nf 3>&1 | cat\nputs after\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&high.stdout),
        "hi\nafter\n",
        "{:?}",
        high.stderr
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the reader waited on a leaked high descriptor: {elapsed:?}"
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
    await_pty_harness(harness);
}

fn background_function_terminal_harness(exec: &MeshExec) -> i32 {
    use std::os::fd::RawFd;

    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    if open_pty_pair(&mut master, &mut slave) != 0
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
    let Some((echoed, _)) = pty_read_until_command_done(master) else {
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

/// How long a pty read waits on silence before calling it a failure.
///
/// Generous on purpose. The shell is *legitimately* quiet for stretches: while
/// it starts up, while a command runs without printing, while a `wait` blocks.
/// Nothing distinguishes that from a wedged shell except how long you are
/// willing to sit there, so the only cost of being wrong in this direction is
/// how slowly a genuinely broken run fails — bounded anyway by the deadline on
/// each reader. Two seconds was too tight: it lost the startup race about twice
/// in a hundred suite runs, reported as the shell never showing a prompt.
const QUIET: libc::c_int = 10_000;

/// `B` — the prompt is drawn and the shell is taking input.
///
/// reedline **repaints**, so this arrives more than once per prompt: a session
/// running one command emits `A B A B C D`. Waiting for the first is still sound
/// for "the shell is up", which is all the callers below want from it, but it is
/// why readiness is the only thing it is used for. Anything that has to line up
/// with a *particular* command waits for [`pty_read_until_command_done`].
const INPUT_READY: &[u8] = b"\x1b]133;B\x1b\\";

/// `D` — the command ended, with its status. The shell writes it once, at the
/// transition, so unlike a prompt it cannot be seen twice for one command.
const COMMAND_DONE: &[u8] = b"\x1b]133;D;";

/// Act as the small piece of terminal-emulator behavior reedline needs while
/// waiting for the shell to start taking input.
fn pty_wait_for_prompt(master: std::os::fd::RawFd) -> bool {
    pty_read_until_one_of(master, &[INPUT_READY]).is_some()
}

/// Read until the shell says the command is over, answering `ESC[6n` on the way,
/// and return what it wrote along with the status it ended on.
///
/// This replaces "read until a prompt appears". A prompt is something reedline
/// *draws*, and redraws: the reader could stop on a repaint of the previous
/// command's prompt and take it for this command's, which is a bug that reached
/// `main` twice. `D` is written once by the shell when the command actually
/// ends, so there is nothing to mistake it for, and it carries the status —
/// which the prompt only ever hinted at through its glyph.
fn pty_read_until_command_done(master: std::os::fd::RawFd) -> Option<(Vec<u8>, u8)> {
    pty_read_until_command_done_within(master, QUIET)
}

/// [`pty_read_until_command_done`], with the wait for the *first* byte bounded by
/// `quiet` rather than by [`QUIET`].
///
/// Only for a caller that has something else to try when nothing arrives — see
/// [`pty_interrupt_until_command_done`]. Everything else wants the long budget,
/// because for those a silent shell is the failure rather than a cue.
fn pty_read_until_command_done_within(
    master: std::os::fd::RawFd,
    quiet: libc::c_int,
) -> Option<(Vec<u8>, u8)> {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut seen = Vec::new();
    // The status digits follow the mark, so the read is not finished until the
    // sequence's terminator has arrived too.
    let complete = |bytes: &[u8]| {
        let start = bytes
            .windows(COMMAND_DONE.len())
            .position(|part| part == COMMAND_DONE)?;
        let rest = &bytes[start + COMMAND_DONE.len()..];
        let end = rest.windows(2).position(|part| part == b"\x1b\\")?;
        std::str::from_utf8(&rest[..end]).ok()?.parse::<u8>().ok()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let done = complete(&seen);
        // Once the mark is in hand, keep draining briefly rather than stopping
        // on it. What follows is the next prompt, and leaving it unread backs the
        // pty up until the shell blocks writing into it — which presents as the
        // shell ignoring whatever is typed next.
        let timeout = if done.is_some() { 50 } else { quiet };
        if unsafe { libc::poll(&mut ready, 1, timeout) } <= 0 {
            return done.map(|status| (seen, status));
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return None;
        }
        let fresh = seen.len().saturating_sub(3);
        seen.extend_from_slice(&chunk[..count as usize]);
        // Answer only queries in the bytes that just arrived. Scanning the whole
        // buffer re-answers every earlier query on every read, and reedline takes
        // the replies as *input* — the line stops being empty, so Ctrl-D no
        // longer exits and a harness waiting on the shell waits forever. The
        // three-byte overlap is for a query split across two reads.
        if seen[fresh..].windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
    }
    complete(&seen).map(|status| (seen, status))
}

/// Read from the PTY until `marker` appears, answering cursor-position queries so
/// reedline keeps going. Used to wait for evidence that a backgrounded body has
/// actually run, rather than checking the moment the prompt returns.
/// Send Ctrl-C until the shell reports a command ending, and return that ending.
///
/// One keystroke is not enough, and the reason is a genuine gap rather than a
/// slow machine. An interactive shell ignores SIGINT except where it has armed
/// itself to catch it — around a foreground job, or around an interruptible read
/// — so a keystroke that lands *between* those windows is discarded by design.
/// Nothing the shell writes marks the moment a read begins: the output of the
/// command before it proves only that the command before it finished, which is
/// the near side of the gap and not the far one.
///
/// So the keystroke repeats until it lands. What is retried is the **stimulus**,
/// not the assertion — the caller still checks the status and the variable
/// exactly as before, and a shell that ignored Ctrl-C properly blocked would
/// keep ignoring it and still fail here on the deadline. A keystroke that
/// arrives once the prompt is back merely cancels an empty line, which is why
/// repeating is safe.
fn pty_interrupt_until_command_done(master: std::os::fd::RawFd) -> Option<(Vec<u8>, u8)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut seen = Vec::new();
    while std::time::Instant::now() < deadline {
        if !pty_write(master, b"\x03") {
            return None;
        }
        // Short, because a keystroke that fell in the gap produces *nothing* and
        // the answer is to send another rather than to keep waiting. The overall
        // budget is the deadline above, so this is not a shorter timeout for the
        // test — only a shorter one per attempt.
        if let Some((window, status)) = pty_read_until_command_done_within(master, 250) {
            seen.extend_from_slice(&window);
            return Some((seen, status));
        }
    }
    None
}

/// Read until every marker has been seen, in one buffer.
///
/// Not a loop over [`pty_wait_for_marker`]: each call there starts a fresh
/// buffer and stops on its first match, so a second call would be reading the
/// leftovers of the first one's output rather than anything new.
fn pty_wait_for_markers(master: std::os::fd::RawFd, markers: &[&str]) -> bool {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut seen = Vec::new();
    let all_present = |seen: &[u8]| {
        markers.iter().all(|marker| {
            let marker = marker.as_bytes();
            seen.windows(marker.len()).any(|part| part == marker)
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if all_present(&seen) {
            return true;
        }
        if unsafe { libc::poll(&mut ready, 1, QUIET) } <= 0 {
            return false;
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return false;
        }
        let fresh = seen.len().saturating_sub(3);
        seen.extend_from_slice(&chunk[..count as usize]);
        // Answer only queries in the bytes that just arrived, for the reason
        // given on `pty_wait_for_marker`.
        if seen[fresh..].windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
    }
    all_present(&seen)
}

fn pty_wait_for_marker(master: std::os::fd::RawFd, marker: &[u8]) -> bool {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut seen = Vec::new();
    // Wall clock rather than a read count, for the reason given on
    // `pty_read_until_any_prompt`: under load the same output arrives in more
    // reads, and a count bounds fragmentation instead of time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if seen.windows(marker.len()).any(|part| part == marker) {
            return true;
        }
        if unsafe { libc::poll(&mut ready, 1, QUIET) } <= 0 {
            return false;
        }
        let mut chunk = [0_u8; 256];
        let count = unsafe { libc::read(master, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count <= 0 {
            return false;
        }
        let fresh = seen.len().saturating_sub(3);
        seen.extend_from_slice(&chunk[..count as usize]);
        // Answer only queries in the bytes that just arrived. Scanning the whole
        // buffer re-answers every earlier query on every read, and reedline takes
        // the replies as *input* — the line stops being empty, so Ctrl-D no
        // longer exits and a harness waiting on the shell waits forever. The
        // three-byte overlap is for a query split across two reads.
        if seen[fresh..].windows(4).any(|part| part == b"\x1b[6n") {
            unsafe { libc::write(master, b"\x1b[1;1R".as_ptr().cast(), 6) };
        }
    }
    seen.windows(marker.len()).any(|part| part == marker)
}

/// Bounded by wall clock rather than by a read count. A prompt costs far more
/// bytes than it looks: reedline repaints the line, so one `mesh$` arrives
/// wrapped in cursor saves, clears and color resets, and a loaded machine hands
/// the same bytes back in more, smaller reads. Counting reads turned that
/// fragmentation into "no prompt" while the prompt was still arriving — the
/// budget was eight, and a passing run was observed using all eight. The
/// per-poll timeouts below are unchanged and still fail fast, so a shell that
/// has genuinely gone quiet is caught as quickly as before; only a stream that
/// keeps delivering gets the extra reads.
/// Read until one of `accept` appears, answering cursor-position queries so
/// reedline keeps going, then drain briefly for whatever trails it.
///
/// Which prompts count has to be the *caller's* choice rather than a check it
/// applies afterwards. Waiting for any prompt and then testing for `mesh$`
/// stopped at the first prompt of either kind, and a failing command leaves
/// `mesh!` on the screen for reedline to repaint while the next line is typed —
/// so the read returned this, having never waited for the `mesh$` it was after:
///
/// ```text
/// puts recovered\r\nmesh: command not found: …\r\n…[1;1H…mesh! …
/// ```
///
/// Bounded by wall clock, not by a count of reads: a prompt costs far more bytes
/// than it looks, since reedline wraps it in cursor saves, clears and color
/// resets, and a loaded machine returns the same bytes in more, smaller reads.
/// The per-poll timeouts still fail fast, so a shell that has genuinely gone
/// quiet is caught as quickly as before.
fn pty_read_until_one_of(master: std::os::fd::RawFd, accept: &[&[u8]]) -> Option<Vec<u8>> {
    let mut ready = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    let seen = |bytes: &[u8]| {
        accept
            .iter()
            .any(|want| bytes.windows(want.len()).any(|part| part == *want))
    };
    let mut prompt = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let found = seen(&prompt);
        let timeout = if found { 50 } else { QUIET };
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
    seen(&prompt).then_some(prompt)
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
    // `wait` rather than an interval chosen to outlast the job: the `cat` below
    // reads what the pipeline wrote, so the pipeline has to be finished, and no
    // fixed sleep is long enough on a machine that is busy enough.
    let out = run_with_input(&format!(
        "sh -c 'sleep 0.05; echo background > {0}/result' | cat & puts foreground\nwait 1\ncat {0}/result\n",
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
    let out = run_with_input(&format!("cat & puts after\n{}jobs\n", await_job(1)));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("[1]"));
}

#[test]
fn background_pipeline_retains_statuses_reaped_on_earlier_prompts() {
    // What is being tested is that a status collected at one table refresh
    // survives to a later one, so the first `jobs` has to happen in the window
    // where the *first* stage has exited and the last has not. Miss that window
    // and the final `Done (7)` proves nothing: it could equally have been
    // collected on the way past, and the test passes without exercising the
    // retention it is named for.
    //
    // Both edges of that window are held open by the pipeline itself, so neither
    // is a guess about the scheduler:
    //
    // - Stage 1 has exited. Stage 2's `cat` sees EOF only once every write end
    //   is gone and stage 1 holds the only one, so the `gone` marker it writes
    //   next cannot appear any earlier.
    // - Stage 2 has not. It then blocks on a `release` marker that only this
    //   script writes, and only after the first `jobs` has run.
    //
    // The second half was a `sleep` first, which is the same bug this file is
    // full of: descheduled for longer than the sleep, stage 2 finishes early,
    // the first `jobs` reaps the whole job and the listing it should have shown
    // is gone.
    //
    // `wait` is not usable here: it takes the job out of the table, so the
    // `Done (7)` this asserts on would never print.
    let dir = fresh_dir("pipeline_retains_status");
    let gone = dir.join("stage-one-gone");
    let release = dir.join("release");
    let out = run_with_input(&format!(
        "g = '{gone}'\n\
         sh -c 'exit 7' | sh -c 'cat > /dev/null; echo x > {gone}; \
         while [ ! -e {release} ]; do sleep 0.02; done' &\n\
         while $g:exists == false {{ sleep 0.02 }}\n\
         jobs\n\
         puts x > {release}\n\
         {wait}jobs\n",
        gone = gone.to_string_lossy(),
        release = release.to_string_lossy(),
        wait = await_job(1)
    ));
    // The listing is the `jobs` builtin's own output and goes to stdout; the
    // `Done` notice is the shell's and goes to stderr. Asserting both on one
    // stream passes only by accident.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("] Running "),
        "the first listing missed the window where stage 1 is gone and stage 2 is not: {stdout:?} {stderr:?}"
    );
    assert!(stderr.contains("Done (7)"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreground_pipeline_retains_statuses_reaped_on_earlier_prompts() {
    let out = run_with_input("sh -c 'exit 7' | sleep 0.2 &\nsleep 0.05\njobs\nfg\nexit\n");
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn wait_lets_a_background_job_finish_before_the_shell_exits() {
    // The shell hangs its jobs up on the way out, so without a wait the work a
    // background job had left to do is simply lost.
    let dir = fresh_dir("wait_background");
    let job = format!(
        "sh -c 'sleep 0.2; echo finished > {}/result' &\n",
        dir.display()
    );
    let hung_up = run_with_input(&job);
    assert_eq!(hung_up.status.code(), Some(0));
    assert!(!dir.join("result").exists());

    let waited = run_with_input(&format!("{job}wait 1\n"));
    assert_eq!(waited.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(dir.join("result")).unwrap(),
        "finished\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_reports_the_jobs_own_status() {
    for reference in ["1", "%1"] {
        let out = run_with_input(&format!(
            "sh -c 'sleep 0.05; exit 7' &\nwait {reference}\nputs $sh.status\n"
        ));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n", "{reference}");
    }
}

#[test]
fn wait_answers_from_a_finished_jobs_record() {
    // Reading `$sh.jobs` polls the job and reaps its pid while keeping the
    // record, so by the time `wait` runs there is no child left to wait for.
    // The status the record already carries is the answer — waiting after the
    // fact reports what waiting through it would have.
    let out = run_with_input(
        "sh -c 'exit 3' &\nsleep 0.2\nputs state=$sh.jobs[1].state\nwait 1\nputs status=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "state=done\nstatus=3\n"
    );
}

#[test]
fn a_waited_job_leaves_the_table() {
    // Its status has already been handed to the caller, so the prompt-time reap
    // must not announce it a second time.
    let out = run_with_input("sh -c 'exit 5' &\nwait 1\njobs\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Done"), "{stderr}");
}

#[test]
fn the_current_and_previous_job_sigils_name_the_last_two() {
    // `%%` and `%+` are the job you most likely mean — the newest — and `%-` is
    // the one behind it. Each job waited for here exits with its own status, so
    // the statuses say which job each sigil actually picked.
    let out = run_with_input(
        "sh -c 'sleep 0.05; exit 4' &\nsh -c 'sleep 0.05; exit 5' &\nwait %-\nputs previous=$sh.status\nwait %+\nputs current=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "previous=4\ncurrent=5\n"
    );

    let aliased = run_with_input("sh -c 'exit 6' &\nwait %%\nputs current=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&aliased.stdout), "current=6\n");
}

#[test]
fn a_job_that_leaves_promotes_the_one_behind_it() {
    // `%+` and `%-` follow the table rather than naming fixed ids. Waiting for
    // the current job of three promotes the previous one into `%+`, and the job
    // behind *that* fills the `%-` it vacated — so both sigils have moved on by
    // one without either being named directly.
    let out = run_with_input(
        "sh -c 'sleep 0.05; exit 4' &\nsh -c 'sleep 0.05; exit 5' &\nsh -c 'sleep 0.05; exit 6' &\nwait %+\nputs newest=$sh.status\nwait %-\nputs behind=$sh.status\nwait %+\nputs promoted=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "newest=6\nbehind=4\npromoted=5\n"
    );
}

#[test]
fn recency_survives_the_current_job_leaving() {
    // Promoting `%-` needs a *third* job to fill the slot it vacates, and once
    // `bg` has moved jobs forward the table's own order no longer says which.
    // Four jobs resumed as 2, 1, 3 leave recency `3 1 2 4`: after job 3 is
    // waited for, `%+` is 1 and `%-` is 2 — while reading the third-most-recent
    // back off registration order picked job 4, which nothing had touched.
    //
    // The jobs outlive the `bg` calls so their process groups are still there
    // to signal, and each carries its own status so the statuses say which job
    // every sigil actually picked.
    let out = run_with_input(
        "sh -c 'sleep 0.4; exit 1' &\nsh -c 'sleep 0.4; exit 2' &\nsh -c 'sleep 0.4; exit 3' &\nsh -c 'sleep 0.4; exit 4' &\nbg 2\nbg 1\nbg 3\nwait %+\nputs head=$sh.status\nwait %-\nputs behind=$sh.status\nwait %+\nputs promoted=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "head=3\nbehind=2\npromoted=1\n"
    );
}

#[test]
fn a_foregrounded_job_that_stops_again_keeps_its_place() {
    // `%prefix` means the most recently *registered* matching command, which it
    // reads off the table's own order. `fg` used to put a job that stopped again
    // back on the end, so an older job could look like the newest one to
    // `%prefix` — and to `$sh.jobs`, which documents itself as insertion-ordered.
    //
    // Job 1 stops at once, is foregrounded, and stops a second time; job 2 is
    // the newer `sh`. `%sh` has to be job 2, and picking the misplaced job 1
    // shows up as its stop status rather than an exit status.
    let out = run_with_input(
        "sh -c 'kill -STOP $$; kill -STOP $$; exit 1' &
sh -c 'sleep 0.6; exit 2' &
sleep 0.3
fg 1
wait %sh
puts matched=$sh.status
",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "matched=2\n");
}

#[test]
fn a_percent_prefix_names_the_most_recent_matching_command() {
    // The most recent match, not the first: two jobs start with `sh`, and the
    // later one is what `%sh` means.
    let out = run_with_input(
        "sh -c 'sleep 0.05; exit 4' &\nsleep 9 &\nsh -c 'sleep 0.05; exit 7' &\nwait %sh\nputs matched=$sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "matched=7\n");

    // An id wins over a prefix, so `%1` stays job 1 even where a command starts
    // with "1".
    let numeric =
        run_with_input("sh -c 'sleep 0.05; exit 3' &\n1234 &\nwait %1\nputs by-id=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&numeric.stdout), "by-id=3\n");
}

#[test]
fn unusable_job_references_say_so() {
    for (reference, needle) in [
        // A prefix nothing matches, and a bare `%`, which names no job at all —
        // `starts_with("")` would otherwise quietly match the newest job.
        ("%nope", "%nope: no such job"),
        // All digits is an id even when it is too large to be one, so it cannot
        // fall through and match a command whose name starts with those digits.
        (
            "%18446744073709551616",
            "%18446744073709551616: no such job",
        ),
        ("%", "%: no such job"),
        // `DESIGN.md` keeps `%?string` for a substring match and defers it, so
        // it is refused by name rather than reported as a missing job. It needs
        // quoting to get this far: `?` is a glob character first.
        (
            "'%?leep'",
            "matching a command by substring is not implemented",
        ),
    ] {
        let out = run_with_input(&format!(
            "18446744073709551616 &\nsleep 9 &\nwait {reference}\nputs after\n"
        ));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{reference}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "after\n",
            "{reference}"
        );
    }
}

#[test]
fn a_background_assignment_binds_a_job_handle() {
    // `j = cmd &` binds the job, not the status of launching it: `$j.pid` is
    // mesh's replacement for bash's `$!`.
    let out = run_with_input(
        "j = sh -c 'sleep 0.3; exit 7' &\nputs id=$j.id\nputs same=$sh.jobs[1].pid/$j.pid\nputs cmd=$j.cmd\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("id=1\n"), "{stdout}");
    let pids = stdout.lines().nth(1).expect("same= line");
    let (left, right) = pids["same=".len()..].split_once('/').expect("two pids");
    assert_eq!(left, right, "the handle and the table disagree: {stdout}");
    assert!(stdout.contains("cmd=sh -c sleep 0.3; exit 7"), "{stdout}");
}

#[test]
fn a_job_handle_reads_the_job_as_it_is_now() {
    // The whole point of a handle over a captured record: `$j.state` is answered
    // from the table at the moment it is read, so it moves on with the job.
    let out = run_with_input(
        "j = sh -c 'sleep 0.2; exit 7' &\nputs first=$j.state\nsleep 0.5\nputs then=$j.state status=$j.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "first=running\nthen=done status=7\n"
    );
}

#[test]
fn a_handle_reads_the_same_however_it_arrives() {
    // Expansion and the expression evaluator resolve members separately, and
    // only expansion knew about handles. So `$j.state` worked while `($j).state`
    // and a handle returned from a function did not — whether a handle could be
    // read depended on how it reached the `.`, not on what it was.
    // Both access forms, since indexing and member access are separate arms and
    // each needed teaching on its own.
    let out = run_with_input(
        "j = sh -c 'sleep 0.2; exit 6' &\na = ($j).state\nputs a=$a\nfunc ident(x) { return $x }\nb = ident($j).state\nputs b=$b\nsleep 0.5\nc = ($j).status\nputs c=$c\nd = ($sh.jobs[1]).state\nputs d=$d\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a=running\nb=running\nc=6\nd=done\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let indexed = run_with_input(
        "j = sh -c 'sleep 0.2; exit 6' &\na = ($j)[\"state\"]\nputs a=$a\nfunc ident(x) { return $x }\nb = ident($j)[\"state\"]\nputs b=$b\nsleep 0.5\nc = ($j)[\"status\"]\nputs c=$c\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&indexed.stdout),
        "a=running\nb=running\nc=6\n",
        "stderr: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );

    // A value that genuinely cannot be indexed still says so.
    let scalar = run_with_input("s = hello\nx = ($s)[\"k\"]\nputs after\n");
    assert!(
        String::from_utf8_lossy(&scalar.stderr).contains("cannot index a scalar value"),
        "{:?}",
        scalar.stderr
    );

    // And a job that has left the table says so here too, rather than reporting
    // that a handle is not a map.
    for access in [".status", "[\"status\"]"] {
        let gone = run_with_input(&format!(
            "j = sh -c 'exit 3' &\nwait $j\nx = ($j){access}\nputs after\n"
        ));
        assert!(
            String::from_utf8_lossy(&gone.stderr).contains("job 1 is no longer in the job table"),
            "{access}: {:?}",
            gone.stderr
        );
        assert_eq!(String::from_utf8_lossy(&gone.stdout), "after\n", "{access}");
    }
    let gone = run_with_input("j = sh -c 'exit 3' &\nwait $j\nx = ($j).status\nputs after\n");
    assert!(
        String::from_utf8_lossy(&gone.stderr).contains("job 1 is no longer in the job table"),
        "{:?}",
        gone.stderr
    );
    assert_eq!(String::from_utf8_lossy(&gone.stdout), "after\n");
}

#[test]
fn a_job_handle_has_no_literal_form() {
    // `:repr` writes the source you would have typed for a value, and a handle
    // has none: the id is the only part of it that could be written, and the
    // only part that means nothing outside this shell's table. Reading it back
    // would hand over whatever job held that id by then.
    let out = run_with_input("j = sleep 9 &\nputs $j:repr\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("a job handle has no literal form"),
        "{:?}",
        out.stderr
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_job_handle_has_no_text_form() {
    // Like a stream handle, and for a reason beyond tidiness: it is what keeps
    // `kill $j` a job where `kill 49001` is a pid, without either guessing.
    let out = run_with_input("j = sleep 9 &\nputs $j\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("a job handle has no text form"),
        "{:?}",
        out.stderr
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_handle_is_a_job_reference() {
    // `DESIGN.md` makes both spellings a handle, and both have to reach the job
    // builtins as one — a handle is refused as an argument everywhere else.
    for reference in ["$j", "$sh.jobs[1]"] {
        let out = run_with_input(&format!(
            "j = sh -c 'sleep 0.1; exit 6' &\nwait {reference}\nputs status=$sh.status\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "status=6\n",
            "{reference}"
        );
    }

    // A map that is not a job record says so rather than being read for a pid.
    let other = run_with_input("m = [a: 1]\nwait $m\nputs after\n");
    assert!(
        String::from_utf8_lossy(&other.stderr).contains("wait: a map is not a job"),
        "{:?}",
        other.stderr
    );
    assert_eq!(String::from_utf8_lossy(&other.stdout), "after\n");
}

#[test]
fn a_handle_to_a_finished_job_says_the_job_is_gone() {
    // Waiting takes the job out of the table, so the handle outlives what it
    // names. That is reported rather than answered with a stale record — the
    // status is `wait`'s own result, which is where to read it.
    let out = run_with_input(
        "j = sh -c 'exit 3' &\nwait $j\nputs waited=$sh.status\nputs $j.status\nputs after\n",
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("job 1 is no longer in the job table"),
        "{:?}",
        out.stderr
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "waited=3\nafter\n");
}

#[test]
fn a_table_handle_stays_live_once_stored() {
    // `$sh.jobs[1]` is a handle in its own right, so storing it has to keep it
    // one. Indexing the table used to yield the record, which froze the moment
    // it was bound — the stored copy said `running` while the table said `done`,
    // which is exactly the staleness a handle exists to avoid.
    let out = run_with_input(
        "sh -c 'sleep 0.2; exit 7' &\nj = $sh.jobs[1]\nsleep 0.5\nputs stored=$j.state table=$sh.jobs[1].state status=$j.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "stored=done table=done status=7\n"
    );

    // And it is still a reference, not just a reader.
    let waited = run_with_input(
        "sh -c 'sleep 0.1; exit 5' &\nj = $sh.jobs[1]\nwait $j\nputs status=$sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&waited.stdout), "status=5\n");
}

#[test]
fn a_redirected_job_builtin_still_takes_a_handle() {
    // A redirection sends the command down a separate path that expands its own
    // argv, so a handle has to survive that one too. It did not: `kill $j` with
    // a redirect reported that the handle had no text form — before even opening
    // the redirect, so the job was left running.
    let dir = fresh_dir("redirected_kill");
    let out = run_with_input(&format!(
        "j = sleep 30 &\nkill $j 2> {}/err\nputs signalled=$sh.status\nwait $j\nputs waited=$sh.status\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "signalled=0\nwaited=143\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kill_signals_a_job_or_a_pid() {
    // A job means its whole process group; the default signal is TERM, so the
    // job reports `128 + 15`.
    let job = run_with_input("j = sleep 30 &\nkill $j\nwait $j\nputs status=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&job.stdout), "status=143\n");

    // Every spelling of a signal a shell's `kill` takes. `-s sigspec` and
    // `-n signum` are the two option forms bash documents beside `-sigspec`.
    for signal in ["-9", "-KILL", "-SIGKILL", "-s KILL", "-n 9", "-n KILL"] {
        let out = run_with_input(&format!(
            "sleep 30 &\nkill {signal} %+\nwait %+\nputs status=$sh.status\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "status=137\n",
            "{signal}"
        );
    }

    // A bare number is a pid, which is the distinction the handle exists to
    // keep: `kill 49001` never means a job.
    let pid = run_with_input("sleep 30 &\nkill $sh.jobs[1].pid\nwait 1\nputs status=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&pid.stdout), "status=143\n");
}

#[test]
fn kill_takes_signal_zero_as_a_liveness_probe() {
    // `kill -0` sends nothing and reports whether the target exists and could be
    // signalled — how a script asks whether something is still running.
    //
    // The sign matters as much as the zero: expanding a job builtin's arguments
    // as typed values turned `-0` into the integer `0`, and `kill 0 $pid` sends
    // to *the caller's own process group*. Arguments other than handles keep the
    // text ordinary expansion gives them, so the option survives as written.
    let out = run_with_input(
        "sleep 5 &\nkill -0 $sh.jobs[1].pid\nputs live=$sh.status\nkill -s 0 $sh.jobs[1].pid\nputs spelled=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "live=0\nspelled=0\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dead = run_with_input("kill -0 999999\nputs dead=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&dead.stdout), "dead=1\n");
    assert!(
        String::from_utf8_lossy(&dead.stderr).contains("kill: 999999"),
        "{:?}",
        dead.stderr
    );
}

#[test]
fn kill_works_from_a_pipeline_stage() {
    // Unlike `fg` / `bg` / `wait`, `kill` neither waits nor touches the terminal,
    // and signalling needs permission rather than parenthood — so a forked stage
    // can do it with the job table it inherited, as bash's `kill` can.
    for reference in ["$j", "%1"] {
        let out = run_with_input(&format!(
            "j = sleep 30 &\nkill {reference} | cat\nputs piped=$sh.status\nwait $j\nputs waited=$sh.status\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "piped=0\nwaited=143\n",
            "{reference}: stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The three that *do* need to be the shell still say so.
    let waited = run_with_input("sleep 5 &\nwait $sh.jobs[1] | cat\nputs w=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&waited.stderr)
            .contains("wait: no job control in a pipeline stage"),
        "{:?}",
        waited.stderr
    );
}

#[test]
fn kill_signals_every_handle_a_spread_produces() {
    // `kill` takes independent targets, and a list of handles spreads into
    // several — so a word naming jobs is converted as a whole rather than only
    // when it happens to produce exactly one value.
    let out = run_with_input(
        "a = sleep 30 &\nb = sleep 30 &\nhs = [$a $b]\nkill ...$hs\nputs spread=$sh.status\nwait $a\nputs a=$sh.status\nwait $b\nputs b=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "spread=0\na=143\nb=143\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn kill_leaves_the_signal_number_to_the_kernel() {
    // Which signal numbers exist is the platform's answer, not a table here: a
    // fixed ceiling rejected valid ones — Linux's `SIGRTMAX` is 64 — and keeping
    // one right would need a per-platform list. An impossible number comes back
    // from `kill` as `EINVAL`, reported against the target like any other
    // failure to signal.
    //
    // Asserted as "not refused as a signal" rather than by number, since the
    // valid range differs per platform and the change is about who decides.
    let out = run_with_input("sleep 30 &\nkill -64 $sh.jobs[1].pid\nputs rt=$sh.status\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("invalid signal"),
        "{:?}",
        out.stderr
    );

    // An unknown *name* still is: no kernel call can make sense of it.
    let named = run_with_input("sleep 5 &\nkill -NOPE %1\nputs bad=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&named.stderr).contains("kill: NOPE: invalid signal"),
        "{:?}",
        named.stderr
    );
    assert_eq!(String::from_utf8_lossy(&named.stdout), "bad=1\n");
}

#[test]
fn kill_cont_puts_the_job_back_to_running() {
    // Continuing a job by hand is `bg` by another spelling, and the table has to
    // agree. Nothing else notices a continue: the poll watches for exits and
    // stops, so a job left marked stopped stayed that way — `jobs` kept saying
    // `Stopped`, and `wait` handed back the cached stop status (147) while the
    // job ran on to its real exit.
    let out = run_with_input(
        "sh -c 'kill -STOP $$; sleep 0.3; exit 5' &\nsleep 0.3\nkill -CONT %1\njobs\nwait 1\nputs waited=$sh.status\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[1] Running"), "{stdout}");
    assert!(stdout.contains("waited=5"), "{stdout}");
}

#[test]
fn a_stopped_job_is_still_watched() {
    // A job marked stopped used to be skipped by every poll, so nothing that
    // happened to it afterwards reached the table. Two ways that shows:
    //
    // Killed where it stands — it stayed listed as stopped for good, and `wait`
    // handed back the stop status (147) rather than how it actually ended.
    //
    // No sleep between the kill and the wait, deliberately. An earlier version
    // of this test had one, which hid a race: the single non-blocking poll `wait`
    // does can run before the kernel has posted the termination, and on a `None`
    // poll the cached stop was what got reported. Repeated, because a race that
    // is merely likely to show would pass often enough to look fixed.
    for _ in 0..10 {
        let killed = run_with_input(
            "sh -c 'kill -STOP $$; sleep 5' &\nsleep 0.3\nkill -KILL %1\nwait 1\nputs waited=$sh.status\n",
        );
        assert_eq!(String::from_utf8_lossy(&killed.stdout), "waited=137\n");
    }

    // Continued by something other than this table — here a `kill -CONT` in a
    // pipeline stage, whose copy of the table dies with the stage. Only asking
    // for `WCONTINUED` makes the parent able to see it at all.
    let continued = run_with_input(
        "sh -c 'kill -STOP $$; sleep 0.3; exit 5' &\nsleep 0.3\nkill -CONT %1 | cat\nsleep 0.5\nwait 1\nputs waited=$sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&continued.stdout), "waited=5\n");

    // And a job genuinely left stopped still reports its stop rather than
    // blocking on something that will not finish.
    let stopped = run_with_input(
        "sh -c 'kill -STOP $$; sleep 5' &\nsleep 0.3\nwait 1\nputs waited=$sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&stopped.stdout), "waited=147\n");
}

#[test]
fn a_bare_wait_reports_the_last_job_to_fail() {
    // bash returns 0 from a bare `wait` whatever happened, which throws away the
    // one thing the caller waited to find out. mesh keeps failure visible, by
    // the rule it already applies to a pipeline: the last failure wins.
    //
    // The jobs finish in id order here, so "last to fail" is job 2 and the 0s on
    // either side of it are what prove a later success does not erase it.
    let out = run_with_input(
        "sh -c \"sleep 0.1; exit 0\" &\n\
         sh -c \"sleep 0.2; exit 5\" &\n\
         sh -c \"sleep 0.3; exit 0\" &\n\
         wait\n\
         puts all=$sh.status\n",
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("all=5"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Nothing failed, so nothing is reported.
    let clean =
        run_with_input("sh -c \"exit 0\" &\nsh -c \"exit 0\" &\nwait\nputs ok=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&clean.stdout).contains("ok=0"),
        "{}",
        String::from_utf8_lossy(&clean.stdout)
    );

    // Several operands answer by the same rule, so `wait 1 2` and a bare `wait`
    // cannot disagree about what waiting for two jobs means.
    let named = run_with_input(
        "sh -c \"sleep 0.1; exit 3\" &\n\
         sh -c \"sleep 0.2; exit 0\" &\n\
         wait 1 2\n\
         puts multi=$sh.status\n",
    );
    assert!(
        String::from_utf8_lossy(&named.stdout).contains("multi=3"),
        "{}",
        String::from_utf8_lossy(&named.stdout)
    );
}

#[test]
fn a_job_exiting_130_is_not_mistaken_for_an_interrupt() {
    // Ctrl-C during a wait reports 130, and a job is perfectly entitled to
    // *exit* 130 — a wrapper around something that was itself interrupted does
    // it routinely. While the interruption travelled back as that number the
    // two were indistinguishable, so an ordinary exit ended the whole wait: the
    // status was 130 rather than the later failure, and job 2 was never waited
    // for at all.
    let out = run_with_input(
        "sh -c \"sleep 0.1; exit 130\" &\n\
         sh -c \"sleep 0.3; exit 4\" &\n\
         wait\n\
         puts all=$sh.status\n\
         jobs\n\
         puts done\n",
    );
    let seen = String::from_utf8_lossy(&out.stdout);
    assert!(seen.contains("all=4"), "{seen}");
    // Nothing left listed: the wait ran to the end of the table rather than
    // stopping at the job that looked like an interrupt.
    assert!(!seen.contains("Running"), "{seen}");
}

#[test]
fn a_bare_wait_reports_a_stopped_job_once() {
    // Waiting for a stopped job by name reports the stop rather than blocking
    // for a continue that is not coming. A bare wait used to skip stopped jobs
    // outright, so the same table answered 0 through one spelling and 147
    // through the other.
    let stopped = run_with_input(
        "sh -c 'kill -STOP $$; sleep 5' &\n\
         sleep 0.3\n\
         wait\n\
         puts all=$sh.status\n",
    );
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("all=147"),
        "{}",
        String::from_utf8_lossy(&stopped.stdout)
    );

    // Reported, not consumed: the job is still there to be continued, and the
    // wait did not block on it either.
    let listed = run_with_input(
        "sh -c 'kill -STOP $$; sleep 0.3; exit 6' &\n\
         sleep 0.3\n\
         wait\n\
         puts all=$sh.status\n\
         kill -CONT %1\n\
         wait 1\n\
         puts then=$sh.status\n",
    );
    let seen = String::from_utf8_lossy(&listed.stdout);
    assert!(seen.contains("all=147"), "{seen}");
    assert!(seen.contains("then=6"), "{seen}");
}

#[test]
fn a_disowned_job_leaves_the_table_and_the_hangup() {
    // `disown` means "not my job any more", and that has to be true of every
    // way the shell would otherwise touch it.
    let out = run_with_input("sh -c \"sleep 5\" &\ndisown\njobs\nputs after=$sh.status\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("after=0"), "{stdout}");
    assert!(
        !stdout.contains("[1] Running"),
        "a disowned job was still listed: {stdout}"
    );

    // Including a bare `wait`, which is the reason it is "every job in the
    // table" rather than "every child the shell owns": disowning is how a script
    // says "not that one", so it must not need a second way to say it. A 3s job
    // that was waited for would blow the harness's patience long before this
    // returns.
    let skipped = run_with_input("sh -c \"sleep 3; exit 9\" &\ndisown\nwait\nputs w=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&skipped.stdout).contains("w=0"),
        "a bare wait waited for a disowned job: {}",
        String::from_utf8_lossy(&skipped.stdout)
    );

    // `-h` is the narrower promise: still the shell's job, just not hung up.
    let kept = run_with_input(
        "sh -c \"sleep 0.2; exit 4\" &\ndisown -h\njobs\nwait\nputs kept=$sh.status\n",
    );
    let listed = String::from_utf8_lossy(&kept.stdout);
    assert!(listed.contains("[1] Running"), "{listed}");
    assert!(listed.contains("kept=4"), "{listed}");
}

#[test]
fn disown_r_leaves_a_job_that_already_finished() {
    // "Running" has to mean not-finished as well as not-stopped. A job whose
    // process has exited but whose status nobody has collected is still in the
    // table with an answer to give, and `-r` giving it up throws that answer
    // away — the `wait` below used to report no such job instead of 7.
    //
    // `JobState` alone cannot tell: a poll deliberately sets a finished job back
    // to `Running` so that a job killed out of a stop reports how it ended
    // rather than the stop, which is exactly what made this look running.
    let out = run_with_input(
        "sh -c \"exit 7\" &\n\
         sleep 0.2\n\
         disown -r\n\
         wait 1\n\
         puts got=$sh.status\n",
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("got=7"),
        "stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // A job that really is running is still given up, so the narrowing did not
    // just turn `-r` off.
    let live = run_with_input("sh -c \"sleep 3\" &\ndisown -r\njobs\nputs after=$sh.status\n");
    let seen = String::from_utf8_lossy(&live.stdout);
    assert!(seen.contains("after=0"), "{seen}");
    assert!(!seen.contains("[1] Running"), "{seen}");
}

#[test]
fn disown_refuses_both_selectors_at_once() {
    // `-a` and `-r` name different sets, so together they say nothing. Taking
    // one silently is how the wrong jobs get given up with no word about it:
    // this used to behave as `-r`, leaving a stopped job owned despite the `-a`.
    let out = run_with_input(
        "sh -c 'kill -STOP $$; sleep 5' &\n\
         sleep 0.3\n\
         disown -a -r\n\
         puts refused=$sh.status\n",
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("disown: -a and -r cannot be combined"),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("refused=1"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn disowning_exempts_a_job_from_the_hangup() {
    // The point of the whole thing: a disowned job outlives the shell. Asserted
    // by what the job *did* after the shell was gone, since "was it signalled"
    // is not observable from inside a shell that has exited.
    let dir = fresh_dir("disown_hangup");
    let check = |script: &str, name: &str| {
        let marker = dir.join(name);
        run_with_input(&format!(
            "sh -c 'sleep 0.6; echo alive > {}' &\n{script}\n",
            marker.display()
        ));
        std::thread::sleep(std::time::Duration::from_millis(1400));
        marker.exists()
    };

    assert!(
        !check("puts started", "plain.txt"),
        "an ordinary job survived the shell's hangup"
    );
    assert!(
        check("disown -h", "kept.txt"),
        "`disown -h` did not exempt the job from the hangup"
    );
    assert!(
        check("disown", "gone.txt"),
        "a disowned job was hung up anyway"
    );

    // A job that is *stopped* when it is given up is the case the exemption
    // cannot cover, under either form. Its group is orphaned once the shell
    // exits, and POSIX has the kernel — not mesh — send SIGHUP then SIGCONT to
    // an orphaned group containing a stopped process. mesh could only prevent
    // that by continuing the group on the way out, which would resume a job the
    // user stopped on purpose, silently, once they can no longer object. So it
    // warns while `bg` is still an answer and lets the job go, as bash and zsh
    // do.
    let stopped = |script: &str, name: &str| {
        let marker = dir.join(name);
        let out = run_with_input(&format!(
            "sh -c 'kill -STOP $$; sleep 0.5; echo alive > {}' &\nsleep 0.4\n{script}\n",
            marker.display()
        ));
        std::thread::sleep(std::time::Duration::from_millis(1600));
        (
            marker.exists(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    for form in ["disown", "disown -h"] {
        let (survived, stderr) = stopped(form, &format!("stopped_{}.txt", form.replace(' ', "_")));
        assert!(
            !survived,
            "{form}: a stopped job cannot outlive the shell — the kernel hangs up \
             its orphaned group, so this passing means the test stopped testing that"
        );
        assert!(
            stderr.contains("is stopped and will not survive the shell"),
            "{form}: gave up a stopped job without warning: {stderr}"
        );
    }

    // `disown -h` keeps the job, so it can stop *after* it was given up — and
    // then the warning at disown time has already been and gone. The exemption
    // is void just the same, since it only ever covered mesh's own hangup and
    // never the kernel's, so the shell says so on the way out rather than let
    // the promise fail in silence.
    //
    // The job stops *itself*, and the shell then `wait`s for it. That is what
    // makes this deterministic: a wait blocks until the job changes state and
    // reports the stop, so by the time it returns the shell has certainly
    // noticed. Two earlier versions signalled from outside instead — `kill
    // -STOP %1` returns as soon as the signal is *sent*, so both were really
    // asserting that the shell happened to look after the process had stopped,
    // which failed about one run in five and then again once under load with a
    // `jobs` probe in between.
    //
    // The first sleep is what keeps the case honest: the job is still running
    // when it is disowned, so no warning is issued there, and the one at exit is
    // the only thing that can report the stop.
    let late = dir.join("late_stop.txt");
    let out = run_with_input(&format!(
        "sh -c 'sleep 0.2; kill -STOP $$; sleep 0.5; echo alive > {}' &\n\
         disown -h\n\
         wait 1\n\
         puts waited=$sh.status\n",
        late.display()
    ));
    std::thread::sleep(std::time::Duration::from_millis(1600));
    let said = String::from_utf8_lossy(&out.stderr);
    let listed = String::from_utf8_lossy(&out.stdout);
    // 128 + SIGSTOP(19): the wait reported the stop, so the shell knows.
    assert!(
        listed.contains("waited=147"),
        "the shell never noticed the stop, so the exit notice was never in \
         question: stdout {listed:?} stderr {said:?}"
    );
    assert!(
        said.contains("[1] is stopped, so it will not survive the shell"),
        "a kept job that stopped after being disowned died without a word: {said}"
    );
    assert!(
        !late.exists(),
        "the exemption cannot outrank the kernel's hangup — if this survived, \
         something is continuing the job and the warning is now wrong"
    );

    // The warning is for the stopped case only: an ordinary disown says nothing.
    let (_, quiet) = {
        let marker = dir.join("quiet.txt");
        let out = run_with_input(&format!(
            "sh -c 'sleep 0.6; echo alive > {}' &\ndisown\n",
            marker.display()
        ));
        ((), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    assert!(!quiet.contains("will not survive"), "{quiet}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_continued_job_becomes_current_however_it_is_noticed() {
    // A continue makes a job current, and it must not matter whether the table
    // did it itself or found out by polling. `bg %2` moves job 2 to the front,
    // then continuing job 1 has to move it back — otherwise `%+` still names 2.
    //
    // Each job exits distinctly, so the status says which one `%+` reached: 5 is
    // job 1, 7 is job 2. Both spellings must agree.
    // Job 2 outlives the wait, so reaching it is a wrong answer that arrives
    // late rather than one that cannot be told from job 1 finishing.
    let script = |cont: &str| {
        format!(
            "sh -c 'kill -STOP $$; sleep 0.4; exit 5' &\n\
             sh -c 'kill -STOP $$; sleep 3; exit 7' &\n\
             sleep 0.3\nbg %2\n{cont}\nsleep 0.6\nwait %+\nputs current=$sh.status\n"
        )
    };

    // The direct form always worked: `JobTable::kill` marks it itself.
    let direct = run_with_input(&script("kill -CONT %1"));
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "current=5\n");

    // From a pipeline stage the fork's table dies with the stage, so the parent
    // only learns of the continue through its own poll. That poll restored
    // `Running` but dropped the recency update, leaving `%+` naming job 2.
    let forked = run_with_input(&script("kill -CONT %1 | cat"));
    assert_eq!(
        String::from_utf8_lossy(&forked.stdout),
        "current=5\n",
        "a continue noticed by polling did not make the job current"
    );
}

#[test]
fn stops_between_command_boundaries_keep_the_order_they_happened_in() {
    // Two jobs stopped from *outside* the shell, with no command boundary
    // between them, so neither stop is noticed by anything the script ran.
    //
    // Job 2 is stopped first and job 1 second, so `%+` is job 1. Marking them in
    // the order the table holds them made it job 2 — the answer that is right
    // about the table and wrong about the world.
    //
    // The 150ms is load-bearing and is the *limit* of the guarantee, not a
    // convenience. A nudge per state change means a shell awake between two
    // stops takes them in order; two that land in the same scheduling interval
    // are both pending before the drain starts, and `waitid` enumerates children
    // in an order of the kernel's choosing. Without the pause this names job 2
    // about nine runs in ten. Nothing portable records when a child stopped, so
    // that case stays arbitrary — `TODO.md` keeps it rather than this test
    // pretending to cover it.
    let out = run_with_input(
        "sh -c \"sleep 30\" &\n\
         sh -c \"sleep 30\" &\n\
         sleep 0.3\n\
         p1 = $sh.jobs[1].pid\n\
         p2 = $sh.jobs[2].pid\n\
         sh -c \"kill -STOP -$p2; sleep 0.15; kill -STOP -$p1\"\n\
         sleep 0.3\n\
         bg %+\n",
    );
    // `bg` names the job it restarted, which is how `%+` is observable at all.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[1] Running"),
        "%+ named the job the table held later, not the one that stopped later: {stderr}"
    );
}

// The shell under test loops forever by design, so it is killed rather than
// waited for — and killed as a *group*, see below. `wait4` is what reaps it.
#[allow(clippy::zombie_processes)]
#[test]
fn a_forked_stage_reaps_the_background_children_it_abandons() {
    // A forked stage that only launches background commands never reaches a
    // foreground wait, a `jobs`, or anything else that drains. Its children are
    // its own to reap, and with nothing reaping them they pile up as zombies
    // until the stage hits its process limit — over 1600 in a second and a half
    // before this fix, and the same on main, so it is older than the reaper.
    // Abandoning a child now drains first, so each launch collects the ones
    // before it.
    //
    // Two things this test has to get right, and got wrong first:
    //
    // The shell runs in its own **process group**, and the whole group is
    // killed. Killing just the shell reparents the looping stage to init, where
    // it keeps forking for the life of the machine — that is not a tidiness
    // point, it exhausted this container's process table while I was developing
    // the fix.
    //
    // And only *this group's* zombies are counted. A global `/proc` scan trips
    // on whatever else the host or the rest of the suite is doing, which is a
    // failure that has nothing to do with the shell under test.
    if !Path::new("/proc/self/stat").exists() {
        return; // reading process state this way is Linux-only
    }
    let mut command = mesh_command();
    command
        .arg("-c")
        .arg("fork { loop { sh -c \"exit 7\" &; x = 1 } }")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setpgid` only moves this child into a group of its own, and is
    // async-signal-safe, which is the bar `pre_exec` sets.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn mesh");
    let group = child.id() as libc::pid_t;
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let zombies = zombies_in_group(group);

    // The group, not the process: everything the shell forked shares it.
    // SAFETY: a negated pid names its process group, which `pre_exec` created.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let mut status = 0;
    // SAFETY: reaping the shell this test spawned.
    unsafe { libc::wait4(group, &mut status, 0, std::ptr::null_mut()) };

    assert!(
        zombies < 50,
        "a stage that only backgrounds left {zombies} zombies behind"
    );
}

// Killed rather than waited for: this shell is left sitting on an open stdin on
// purpose, so it never exits on its own.
#[allow(clippy::zombie_processes)]
#[test]
fn a_disowned_child_is_reaped_with_no_job_left_to_notice() {
    // A plain `disown` empties the table but keeps the child, and nothing was
    // left to collect it: the SIGCHLD handler only forwards, and the only other
    // thing that drains is the job table — which the prompt skipped entirely
    // when there were no jobs. So a disowned child stayed `<defunct>` for the
    // rest of the session, contradicting the one thing `disown` still promises
    // about it.
    //
    // The commands after the wait are **builtins**, deliberately. Anything that
    // forks runs the wait path, which drains as a side effect and would hide
    // this; a shell doing only builtin work is what leaves the child sitting
    // there.
    if !Path::new("/proc/self/stat").exists() {
        return; // reading process state this way is Linux-only
    }
    let mut command = mesh_command();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setpgid` only moves this child into a group of its own, and is
    // async-signal-safe, which is the bar `pre_exec` sets. The group is what
    // scopes the zombie count to this shell.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn mesh");
    let group = child.id() as libc::pid_t;
    let mut stdin = child.stdin.take().expect("mesh stdin");

    let _ = writeln!(stdin, "sh -c \"sleep 0.3\" &");
    let _ = writeln!(stdin, "disown");
    let _ = stdin.flush();
    // Long enough for the child to exit with the table already empty.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    for _ in 0..3 {
        let _ = writeln!(stdin, "puts .");
    }
    let _ = stdin.flush();
    std::thread::sleep(std::time::Duration::from_millis(800));

    // Counted by parent, not by group: a background job gets a process group of
    // its own, so the shell's group never contains this zombie and counting that
    // way would pass whatever the shell did.
    let zombies = zombie_children_of(group);

    // The group, not just the shell: killing only the shell would reparent
    // anything it still had running.
    // SAFETY: a negated pid names its process group, which `pre_exec` created.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let mut status = 0;
    // SAFETY: reaping the shell this test spawned.
    unsafe { libc::wait4(group, &mut status, 0, std::ptr::null_mut()) };

    assert_eq!(
        zombies, 0,
        "a disowned child was left unreaped by a shell with no jobs"
    );
}

/// Zombie *children* of `parent`, read out of `/proc`.
///
/// By parent rather than by process group, which is what a disowned job needs:
/// a background job is given a group of its own, so its zombie is not in the
/// shell's group and [`zombies_in_group`] cannot see it. Parentage is the thing
/// that actually says "this is the shell's to reap".
fn zombie_children_of(parent: libc::pid_t) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    let mut zombies = 0;
    for entry in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((_, rest)) = text.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or("");
        let ppid: libc::pid_t = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        if state.starts_with('Z') && ppid == parent {
            zombies += 1;
        }
    }
    zombies
}

/// Zombies whose process group is `group`, read out of `/proc`.
///
/// By group rather than globally, so the count is about the shell under test
/// and not about whatever else the host happens to be running.
fn zombies_in_group(group: libc::pid_t) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    let mut zombies = 0;
    for entry in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` can contain spaces and parentheses, so the fields after it are
        // taken from the last ')': state, ppid, pgrp.
        let Some((_, rest)) = text.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or("");
        let _ppid = fields.next();
        let pgrp: libc::pid_t = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        if state.starts_with('Z') && pgrp == group {
            zombies += 1;
        }
    }
    zombies
}

#[test]
fn a_fork_block_still_notices_what_happens_to_other_jobs() {
    // Every blocking wait has to be cut short when a child changes state, or the
    // changes pile up and are drained together afterwards — at which point their
    // order is gone.
    //
    // The catcher was installed in `wait_outcomes_until`, which covers pipelines
    // and jobs but not `fork_and_wait`: that reaches the blocking wait directly.
    // So a `fork { … }` block ran under the startup disposition, nothing woke it,
    // and two stops 300ms apart arrived as one batch. `bg %+` named the wrong job
    // six runs in eight.
    let out = run_with_input(
        "sh -c \"sleep 30\" &\n\
         sh -c \"sleep 30\" &\n\
         sleep 0.3\n\
         p1 = $sh.jobs[1].pid\n\
         p2 = $sh.jobs[2].pid\n\
         sh -c \"sleep 0.2; kill -STOP -$p2; sleep 0.3; kill -STOP -$p1\" &\n\
         fork { sleep 1 }\n\
         bg %+\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[1] Running"),
        "a wait outside the pipeline path did not notice the stops as they happened: {stderr}"
    );
}

#[test]
fn a_stop_and_the_continue_after_it_are_both_read_at_once() {
    // The store keeps a *queue* per pid, because a job can stop and be continued
    // again between two polls. Answering with the first of those left `$sh.jobs`
    // reporting `stopped` for a job that was running, and only a *second* read
    // reported the truth — reading is not supposed to be what moves the shell on.
    //
    // The gap matters: continued quickly enough, the kernel discards the pending
    // stop notification and there is only ever one transition to find. A second
    // of separation is what guarantees the stop is observed before the continue,
    // which is the case that queues both.
    let out = run_with_input(
        "sh -c \"sleep 30\" &\n\
         sleep 0.3\n\
         p = $sh.jobs[1].pid\n\
         sh -c \"kill -STOP -$p; sleep 1; kill -CONT -$p; sleep 0.3\"\n\
         puts first=$sh.jobs[1].state\n\
         puts second=$sh.jobs[1].state\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("first=running"),
        "the first read reported a stop the continue had already undone: {stdout}"
    );
    assert!(stdout.contains("second=running"), "{stdout}");
}

#[test]
fn the_shell_holds_no_descriptor_a_script_can_reach() {
    // The reaper wanted somewhere to be woken from, and a self-pipe put a
    // descriptor of the shell's own into the numbering scripts address. It
    // answered `>&3`, then `>&101` once moved, then collided with relocation
    // targets, then could not relocate under a low limit, and was finally
    // reachable *by path* through `/dev/fd/100` — seven findings for one
    // descriptor. It is gone: the wait is cut short by `pthread_kill` to the
    // waiting thread, which needs no namespace at all.
    //
    // So this is the invariant that bought, asserted the only way it can be:
    // every way of naming a descriptor the shell might have kept says nothing
    // is there.
    for spelling in [
        "puts hi >&100",
        "puts hi >&101",
        "cat 2>&101",
        "cat 0<&100",
        "puts hi >&3",
    ] {
        let out = run_with_input(&format!("{spelling}\nputs status=$sh.status\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Bad file descriptor"),
            "{spelling}: {stderr}"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("status=1"),
            "{spelling}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // Reachable by path, not just by number: `/dev/fd/N` and `/proc/self/fd/N`
    // are opened as paths, so no amount of care about descriptor *names* covers
    // them. With the pipe gone there is nothing for them to alias, and they
    // report what a fresh shell does.
    for path in ["/dev/fd/100", "/proc/self/fd/100", "/dev/fd/101"] {
        let out = run_with_input(&format!("cat < {path}\nputs after\n"));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("after"),
            "{path} did not return: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // And the shell still hears about its children, which is what any of this
    // was in aid of.
    let out = run_with_input("sh -c \"sleep 0.2; exit 6\" &\nwait 1\nputs waited=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("waited=6"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `run_at_descriptor_limit` with `SIGCHLD` inherited as ignored — a shell whose
/// children the kernel reaps behind its back, which is the state an ignore
/// inherited from whatever started mesh actually produces.
fn run_with_sigchld_ignored(limit: libc::rlim_t, command_line: &str) -> Output {
    let mut command = mesh_command();
    command.arg("-c").arg(command_line);
    // SAFETY: both calls only change this child's own limits and dispositions,
    // and both are async-signal-safe, which is the bar `pre_exec` sets.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::signal(libc::SIGCHLD, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.output().expect("run mesh")
}

#[test]
fn an_inherited_sigchld_ignore_does_not_hang_the_shell() {
    // A shell started with `SIGCHLD` ignored has its children auto-reaped by the
    // kernel, so no transition ever reaches the store — and a wait still told the
    // pid is owned then waits for news that cannot come.
    //
    // Installing the handler used to sit *after* the pipe, so a failed `pipe`
    // returned early and left the inherited ignore in place. `mesh -c true` hung
    // outright. The disposition is now set first and unconditionally: losing the
    // pipe costs a slower wait, losing this costs every wait there is.
    let out = run_with_sigchld_ignored(4, "true");
    assert!(out.status.success(), "{:?}", out.status);

    // And with room for the pipe, so the ignore is the only thing wrong: the
    // status still has to come back, which is what proves the child was waited
    // for rather than auto-reaped behind the shell's back.
    let out = run_with_sigchld_ignored(64, "sh -c \"exit 7\"");
    assert_eq!(out.status.code(), Some(7), "{:?}", out.status);
}

#[test]
fn a_redirection_naming_a_high_descriptor_behaves() {
    // The shell's own descriptors move out of the way of a redirection, and
    // *when* they move is as load-bearing as whether. Both of these were the
    // same mistake seen from two sides.
    let dir = fresh_dir("step_aside_order");

    // Moving them during descriptor resolution was already too late for the
    // in-shell path, which snapshots which targets it holds open before that: it
    // saw an endpoint on fd 100, the move closed it, and the save then ran on a
    // closed descriptor. `puts hi` still goes to stdout — redirecting fd 100
    // says nothing about where `puts` writes — so the file is empty and the
    // status is what this is about.
    let empty = dir.join("empty.txt");
    let out = run_with_input(&format!("puts hi 100> {}\n", empty.display()));
    assert!(out.status.success(), "{:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    assert!(empty.exists(), "the redirection did not create its file");

    // And moving them one at a time let an endpoint land on a number a *later*
    // redirection names, or one an earlier redirection had already probed as
    // free. `2>&102` was checked first and found 102 unused; relocating the
    // endpoint at 100 then put it *on* 102, so the duplication succeeded — and
    // the `>` went on to truncate a file it should never have opened.
    let guarded = dir.join("guarded.txt");
    std::fs::write(&guarded, "PRE-EXISTING").unwrap();
    let out = run_with_input(&format!(
        "sh -c true 2>&102 100> {}\nputs after\n",
        guarded.display()
    ));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&guarded).unwrap(),
        "PRE-EXISTING",
        "a file was truncated by a redirection that should have failed first"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_blocked_sigchld_does_not_slow_every_wait() {
    // A disposition is not a delivery. A launcher that exec'd mesh with
    // `SIGCHLD` blocked passes that mask on, so the handler never runs and the
    // nudge pipe never becomes readable — leaving every wait to the poll timeout
    // that exists only as a backstop. A 0.05s sleep took nearly two seconds.
    //
    // Wall clock is the right measure here, unlike the spin test: this bug cost
    // latency rather than CPU.
    let mut command = mesh_command();
    command.arg("-c").arg("sleep 0.05");
    // SAFETY: `pthread_sigmask` only changes this child's own mask, and is
    // async-signal-safe, which is the bar `pre_exec` sets.
    unsafe {
        command.pre_exec(|| {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGCHLD);
            if libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let start = std::time::Instant::now();
    let out = command.output().expect("run mesh");
    let elapsed = start.elapsed();
    assert!(out.status.success(), "{:?}", out.status);
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "an inherited SIGCHLD block left the wait on its backstop: {elapsed:?}"
    );
}

#[test]
fn a_low_descriptor_limit_exposes_nothing_of_the_shells() {
    // A tight `RLIMIT_NOFILE` used to be the worst case for the shell's own
    // plumbing: the nudge pipe could not get clear of the low numbers, so
    // `puts hi >&4` returned 0 and fed `hi` to the next drain. With no pipe to
    // place there is nothing left for a limit to squeeze, and this is the
    // ordinary `EBADF` — asserted at a limit low enough to have broken it.
    let out = run_at_descriptor_limit(8, "puts hi >&4");
    assert_eq!(out.status.code(), Some(1), "{:?}", out.status);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "the output went somewhere other than stdout: {:?}",
        out.stdout
    );
}

// `wait4` *is* this test's wait, and it is the one that has to be used: the
// measurement is the child's CPU, which `Child::wait` does not report.
#[allow(clippy::zombie_processes)]
#[test]
fn a_wait_under_a_descriptor_limit_does_not_spin() {
    // `pipe` can fail — a low `RLIMIT_NOFILE` is enough — and then there is no
    // descriptor to sleep on. Returning "no news" immediately turned every
    // foreground wait into a tight drain loop that burned a core for the whole
    // life of the child. The wait still has to *work*, just more slowly.
    //
    // Measured as CPU rather than wall clock: the bug did not make the shell
    // slower, it made it hot. The threshold is far above what the fix costs
    // (hundredths of a second) and far below a spin (a full second per second).
    let mut command = mesh_command();
    command.arg("-c").arg("sleep 1");
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 4,
                rlim_max: 4,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn mesh");
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::wait4(pid, &mut status, 0, &mut usage) },
        pid,
        "wait4 on the shell"
    );
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    let cpu = usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1e6
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1e6;
    assert!(
        cpu < 0.5,
        "a wait with no nudge pipe spun: {cpu:.3}s of CPU for a one-second sleep"
    );
}

// The fork here is the point of the test, and the shell it exec's is waited for
// by `wait4` below — `Child::wait` cannot express "wait for the process this
// helper turned into".
#[allow(clippy::zombie_processes)]
#[test]
fn a_child_inherited_from_a_parent_does_not_block_the_shell() {
    // mesh can be exec'd by a process that already has a live child of its own.
    // That child is not the shell's to reap, and it must not be able to stand in
    // the way of the ones that are.
    //
    // Discovering which child changed with `waitid(P_ALL, …, WNOWAIT)` and then
    // reaping it only if owned looks careful and deadlocks: `WNOWAIT` leaves the
    // unowned transition pending, so every probe answers with the same pid and
    // the shell's own children are never reached. This hung indefinitely.
    let mut command = mesh_command();
    command.arg("-c").arg("sleep 0.2");
    // SAFETY: `fork` here creates a child that exits on its own; only
    // async-signal-safe calls run in it, which is the bar `pre_exec` sets.
    unsafe {
        command.pre_exec(|| {
            if libc::fork() == 0 {
                // Outlives the exec, then leaves a transition pending that
                // belongs to nobody the shell knows about.
                libc::usleep(50_000);
                libc::_exit(0);
            }
            Ok(())
        });
    }
    let start = std::time::Instant::now();
    let out = command.output().expect("run mesh");
    assert!(out.status.success(), "{:?}", out.status);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "an inherited child blocked the drain: {:?}",
        start.elapsed()
    );
}

#[test]
fn a_child_the_shell_does_not_own_is_left_alone() {
    // The drain is the only caller of `waitpid` now, and it reaps by asking for
    // *any* child. Anything the shell did not spawn itself — the completion
    // helper waits on its own with `Child::wait` — must still find its child
    // waitable, so the drain identifies a pid before consuming it and steps over
    // one it does not own.
    //
    // `fork` gives the shell a child of its own to reap while an unrelated one is
    // outstanding; both statuses have to survive.
    let out = run_with_input(
        "fork { exit 3 }\nputs forked=$sh.status\nsh -c \"exit 4\"\nputs plain=$sh.status\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("forked=3"), "{stdout}");
    assert!(stdout.contains("plain=4"), "{stdout}");
}

#[test]
fn a_map_cannot_forge_a_job_handle() {
    // `m = [id: 1]` is ordinary data. Reading an `id` out of any map would make a
    // handle forgeable, and signalling a job on the strength of a field name is
    // exactly what having a distinct handle value is for.
    let out = run_with_input("sleep 30 &\nm = [id: 1]\nkill $m\nputs forged=$sh.status\njobs\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("forged=1"), "{stdout}");
    assert!(
        stdout.contains("[1] Running"),
        "the forged map signalled a job: {stdout}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("kill: a map is not a job"),
        "{:?}",
        out.stderr
    );

    // The real thing still reaches the job, since the table publishes handles.
    let real = run_with_input("sleep 30 &\nkill $sh.jobs[1]\nwait 1\nputs real=$sh.status\n");
    assert_eq!(String::from_utf8_lossy(&real.stdout), "real=143\n");
}

#[test]
fn kill_knows_the_posix_signal_names() {
    // The table used to hold a subset, so ordinary names were refused as
    // invalid. These are POSIX, so every platform mesh builds for has them.
    for name in ["TRAP", "BUS", "URG", "XCPU", "SYS", "SIGVTALRM"] {
        let out = run_with_input(&format!("sleep 9 &\nkill -{name} %1\nputs after\n"));
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("invalid signal"),
            "{name}: {:?}",
            out.stderr
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{name}");
    }
}

#[test]
fn kill_takes_the_option_terminator_after_a_signal() {
    // `--` ends the options wherever it sits among them, and bash documents the
    // combined form. Left in place it became a target of its own: the signal
    // still landed, but `kill` reported `--: no such job` and returned 1, so a
    // script checking the status saw a failure that had not happened.
    for options in ["-s TERM --", "-9 --", "-n 15 --", "--"] {
        let out = run_with_input(&format!(
            "sleep 30 &\nkill {options} %1\nputs signalled=$sh.status\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "signalled=0\n",
            "{options}: stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A terminator with nothing after it still names no target.
    let empty = run_with_input("kill --\nputs only=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&empty.stderr).contains("expected a job or a pid"),
        "{:?}",
        empty.stderr
    );
    assert_eq!(String::from_utf8_lossy(&empty.stdout), "only=1\n");
}

#[test]
fn kill_reports_what_it_cannot_do() {
    for (line, needle) in [
        ("kill", "kill: expected a job or a pid"),
        ("sleep 9 &\nkill -NOPE %1", "kill: NOPE: invalid signal"),
        ("sleep 9 &\nkill %9", "kill: %9: no such job"),
    ] {
        let out = run_with_input(&format!("{line}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{line:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{line:?}");
    }
}

#[test]
fn wait_takes_a_job_or_all_of_them() {
    // Bare `wait` used to be refused: it means bash's "every child", `fg`'s
    // no-operand form means "the most recent one", and the aggregate status was
    // undecided. It is decided now — every job in the table, last failure wins —
    // so the bare form waits rather than complaining, and a job that finished
    // cleanly leaves the status alone.
    let bare = run_with_input("sleep 0.05 &\nwait\nputs $sh.status\n");
    assert!(
        !String::from_utf8_lossy(&bare.stderr).contains("expected a job"),
        "{:?}",
        bare.stderr
    );
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "0\n");

    // With nothing to wait for it is still not an error: "everything finished"
    // is true of an empty table.
    let empty = run_with_input("wait\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&empty.stdout), "0\n");

    // A named job that does not exist is still a mistake worth reporting.
    let unknown = run_with_input("sleep 0.05 &\nwait 9\nputs $sh.status\n");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("wait: 9: no such job"),
        "{:?}",
        unknown.stderr
    );
    assert_eq!(String::from_utf8_lossy(&unknown.stdout), "1\n");
}

#[test]
fn a_noninteractive_wait_keeps_an_inherited_sigint_ignore() {
    // A batch parent that ignores SIGINT hands that ignore to mesh, and means it
    // to hold: interrupts are not to take effect in what it launched. Only the
    // *interactive* shell ignores SIGINT on its own account, so only there does
    // a wait need a catcher to stay escapable. Reading the disposition alone
    // could not tell the two apart, and abandoning the wait here would cut the
    // job short — the shell hangs it up on the way out.
    use std::os::unix::process::CommandExt;

    let mut command = mesh_command();
    command
        .arg("-c")
        .arg("sh -c 'sleep 2; echo finished' &\nwait 1\nputs status=$sh.status\n")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: `signal` is async-signal-safe and this runs in the forked child
    // between fork and exec, where it is the only thread.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            Ok(())
        })
    };
    let child = command.spawn().expect("spawn mesh");

    // Whenever it lands, the interrupt has to be ignored, so the timing here
    // decides only *which* part of the wait is covered, never the outcome.
    std::thread::sleep(std::time::Duration::from_millis(500));
    // SAFETY: signalling a live child of this process.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };

    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "finished\nstatus=0\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn wait_refuses_in_a_pipeline_stage() {
    // A forked stage is not the parent of the shell's jobs, so its `waitpid`
    // would fail with ECHILD rather than wait for anything — the same reason
    // `fg` and `bg` refuse there.
    let out = run_with_input("sleep 0.05 &\nwait 1 | cat\nputs $sh.status\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("wait: no job control in a pipeline stage"),
        "{:?}",
        out.stderr
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn fg_hands_back_a_finished_jobs_status() {
    // A finished job has no process group left to continue, so `fg` used to
    // signal into the void and fail with ESRCH. Since reading `$sh.jobs` is
    // enough to get here, an observation of the table decided what `fg` did.
    let out = run_with_input(
        "sh -c 'exit 6' &\nsleep 0.2\nputs state=$sh.jobs[1].state\nfg\nputs status=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "state=done\nstatus=6\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[1] Done (6)"), "{stderr}");
    assert!(!stderr.contains("No such process"), "{stderr}");
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
            "cd {}\nfunc noop() {{ true }}\nfunc shows() {{ jobs }}\n             sleep 0.05 &\n{}{pipeline}\n",
            dir.display(),
            await_job(1)
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
    let failed = run_with_input(&format!(
        "sleep 0.05 &\n{}jobs 2> /missing/log | cat\nputs after\n",
        await_job(1)
    ));
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("/missing/log"), "{stderr:?}");
    assert!(stderr.contains("] Done (0) sleep 0.05"), "{stderr:?}");
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "after\n");

    // And a backgrounded stage, whose targets are opened in the child.
    let backgrounded = run_with_input(&format!(
        "sleep 0.05 &\n{}jobs 2> /missing/log &\n{}",
        await_job(1),
        await_job(2)
    ));
    assert!(
        String::from_utf8_lossy(&backgrounded.stderr).contains("] Done (0) sleep 0.05"),
        "{:?}",
        backgrounded.stderr
    );

    // Reaping removes finished jobs, so a stage that cannot look at the table must
    // not trigger it: `puts hi | cat` would otherwise take a completed job out
    // from under a later `fg`, which the unpiped `puts hi` leaves alone.
    let unrelated = run_with_input(&format!(
        "sleep 0.05 &\n{}puts hi | cat\nfg\nputs end\n",
        await_job(1)
    ));
    assert!(
        !String::from_utf8_lossy(&unrelated.stderr).contains("no current job"),
        "an unrelated stage must not reap: {:?}",
        unrelated.stderr
    );
    assert_eq!(String::from_utf8_lossy(&unrelated.stdout), "hi\nend\n");

    // A nested `jobs` still reads a fresh table, which is what the refresh is for.
    let nested = |tail: &str| {
        run_with_input(&format!(
            "sleep 0.05 &\n{}func f() {{ jobs }}\nf{tail}\n",
            await_job(1)
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
    // Both stage kinds defer their opens when backgrounded — each to its own
    // fork — so both have to read fd 1's fate from the redirections rather than
    // from an opened file.
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
fn backgrounding_keeps_a_pipe_the_redirections_leave_on_it() {
    // Where a backgrounded stage's fd 1 ends up used to be read off `cmd.redirs`
    // — "does anything name fd 1?" — which trips on the `>` in both spellings
    // below even though source order puts the pipe back afterwards. mesh printed
    // `low` straight past the pipeline where bash pipes `LOW`. The shell now
    // resolves the stage's redirections *without opening anything* (the opens
    // belong to the child) and asks where they leave the pipe.
    let dir = fresh_dir("background_kept_pipe");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    // Both stage kinds: an in-shell function and an external, each of which
    // opens its targets in its own fork.
    for (label, low, quiet) in [
        ("function", "low", "quiet"),
        ("external", "sh -c 'echo low'", "sh -c 'echo low >&2'"),
    ] {
        // `> file` moves stdout off the pipe and `1>&3` puts it back, so the
        // pipeline gets the output and the file stays empty.
        let out = run_with_input(&format!(
            "cd {}\nfunc low() {{ puts low }}\n{low} 3>&1 > moved 1>&3 | tr a-z A-Z &\nsleep 0.4\n",
            dir.display()
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "LOW\n",
            "{label}: {:?}",
            out.stderr
        );
        assert_eq!(read("moved"), "", "{label}: the pipe took the file's bytes");

        // The mirror: stdout ends on the file and the pipe is held by stderr
        // alone, which still has to be fed to the next stage.
        let out = run_with_input(&format!(
            "cd {}\nfunc quiet() {{ sh -c 'echo low >&2' }}\n\
             {quiet} 2>&1 > filed | tr a-z A-Z &\nsleep 0.4\n",
            dir.display()
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "LOW\n",
            "{label}: {:?}",
            out.stderr
        );

        // And the case the old rule got right, which the new one must keep:
        // nothing is left on the pipe, so the next stage reads end-of-file.
        let _ = std::fs::remove_file(dir.join("all"));
        let out = run_with_input(&format!(
            "cd {}\nfunc low() {{ puts low }}\n{low} > all | tr a-z A-Z &\nsleep 0.4\n",
            dir.display()
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "{label}: {:?}",
            out.stderr
        );
        assert_eq!(read("all"), "low\n", "{label}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_merged_stderr_follows_stdout_wherever_it_went() {
    // `|&` is `2>&1` appended after the stage's own redirections, so it copies
    // wherever stdout *finally* points. Carried as a flag beside the
    // redirections rather than as the duplication it is, every destination
    // stdout could have needed an arm of its own — and the incoming pipe was the
    // arm nobody wrote, so `ERR` went to the shell's stderr instead of into the
    // pipeline. bash emits nothing here: `1<&0` puts stdout on a read end, and
    // the copy puts stderr there too, so neither write goes anywhere.
    let out = run_with_input("printf x | sh -c 'printf ERR >&2' 1<&0 |& cat\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("ERR"),
        "the merged stderr escaped the pipeline: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    // Both stage kinds: an in-shell function and an external, each of which
    // opens its targets in its own fork. The merge has to happen after the
    // targets in each.
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

    // And a bare `|&`, with no redirection of the stage's own. Whether a
    // backgrounded stage had anything to apply after the fork was asked of
    // `cmd.redirs`, which does not carry the `2>&1` that `|&` *is*, so this one
    // spelling was skipped and stderr stayed on the shell's.
    for (label, stage) in [("function", "erring"), ("external", "sh -c 'echo err >&2'")] {
        let out = run_with_input(&format!(
            "func erring() {{ sh -c 'echo err >&2' }}\n{stage} |& tr a-z A-Z &\nsleep 0.4\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "ERR\n",
            "{label}: the merged stderr never reached the next stage: {:?}",
            out.stderr
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("err"),
            "{label}: it escaped to the shell's stderr: {:?}",
            out.stderr
        );
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
         show $ys | tr a-z A-Z\nshow $ys > out\ncat out\n",
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
    let finished = run_with_input(&format!("sleep 0.05 &\n{}jobs | cat\n", await_job(1)));
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
    // The background path opens its targets in the forked child rather than in
    // the shell, so the descriptor has to survive that hand-off as well as the
    // direction does.
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
fn a_descriptor_above_two_can_be_redirected() {
    let dir = fresh_dir("high_descriptors");
    let source = dir.join("in.txt");
    std::fs::write(&source, "from-three\n").unwrap();
    let sink = dir.join("out.txt");
    let ninth = dir.join("nine.txt");

    // Reading and writing through a descriptor the command names itself. `3` is
    // the interesting number: a freshly opened file often *lands* on fd 3, and
    // `dup2` onto the descriptor a file already occupies is a no-op that leaves
    // `FD_CLOEXEC` set — so this passed through the shell and then vanished at
    // `exec`, while a higher descriptor worked.
    let out = run_with_input(&format!(
        "sh -c 'read line <&3; echo $line' 3< {}\n",
        source.to_string_lossy()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "from-three\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_with_input(&format!(
        "sh -c 'echo out-via-3 >&3' 3> {}\n",
        sink.to_string_lossy()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&sink).unwrap(), "out-via-3\n");

    // Well clear of the standard three, so the fix is not just about fd 3.
    let out = run_with_input(&format!(
        "sh -c 'echo nine >&9' 9> {}\n",
        ninth.to_string_lossy()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&ninth).unwrap(), "nine\n");
}

#[test]
fn a_high_descriptor_reaches_every_kind_of_command() {
    // The external pipeline stage is only one of four routes a redirection can
    // take, and each installs descriptors its own way. Testing just that one is
    // what let a redirected function and a backgrounded command keep failing on
    // fd 3 after the first three worked.
    let dir = fresh_dir("high_fd_routes");

    // A function: redirections are applied to the shell's own descriptors and
    // restored afterwards. `dup`-ing fd 3 aside fails with `EBADF` when nothing
    // has it open, which is not a failure — the redirection creates it.
    let via_function = dir.join("fn.txt");
    let out = run_with_input(&format!(
        "func f() {{ sh -c 'echo fn >&3' }}\nf 3> {}\n",
        via_function.to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(&via_function).unwrap_or_default(),
        "fn\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And the descriptor goes away again with the redirection, rather than being
    // left open on the shell.
    let out = run_with_input(&format!(
        "func f() {{ puts x }}\nf 3> {}\nsh -c 'echo late >&3' 2>/dev/null || puts closed\n",
        dir.join("restore.txt").to_string_lossy()
    ));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("closed"),
        "fd 3 outlived its redirection: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // A backgrounded command, whose targets are opened by a helper reached
    // through argv — which split the opened files with a catch-all that put
    // anything above stderr *onto* stderr.
    let via_background = dir.join("bg.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo bg >&3' 3> {} &\nsleep 0.4\n",
        via_background.to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(&via_background).unwrap_or_default(),
        "bg\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A pipeline stage, and a builtin that simply ignores the descriptor.
    let via_stage = dir.join("stage.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo staged >&3' 3> {} | cat\nputs hi 3> {}\n",
        via_stage.to_string_lossy(),
        dir.join("builtin.txt").to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(&via_stage).unwrap_or_default(),
        "staged\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
}

#[test]
fn two_high_descriptors_keep_their_own_targets() {
    // An open takes the lowest free descriptor, so the file for fd 4 lands *on*
    // fd 3 — and installing fd 3 first overwrote it before anything copied it to
    // fd 4, sending both streams to the first file. Both orderings collide, so
    // each is checked.
    let dir = fresh_dir("high_fd_pair");
    for (first, second) in [("4", "3"), ("3", "4")] {
        let three = dir.join(format!("three-{first}.txt"));
        let four = dir.join(format!("four-{first}.txt"));
        let target = |fd: &str| if fd == "3" { &three } else { &four };
        let out = run_with_input(&format!(
            "sh -c 'echo three >&3; echo four >&4' {first}> {} {second}> {}\n",
            target(first).to_string_lossy(),
            target(second).to_string_lossy()
        ));
        assert_eq!(
            std::fs::read_to_string(&three).unwrap_or_default(),
            "three\n",
            "{first} first: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&four).unwrap_or_default(),
            "four\n",
            "{first} first: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The same collision through a function, whose stage installs descriptors
    // itself rather than through `Command`.
    let three = dir.join("fn-three.txt");
    let four = dir.join("fn-four.txt");
    let out = run_with_input(&format!(
        "func f() {{ sh -c 'echo three >&3; echo four >&4' }}\nf 4> {} 3> {}\n",
        four.to_string_lossy(),
        three.to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(&three).unwrap_or_default(),
        "three\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&four).unwrap_or_default(),
        "four\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_high_descriptor_can_hold_a_pipe() {
    // A pipe is not a file, so the resolution that turns destinations into files
    // has nothing to hand fd 3 — it was simply dropped, and the command found
    // `EBADF` on a descriptor the redirection said it had opened. Only stdout
    // and stderr were carried to the pipe by name.
    let out = run_with_input("sh -c 'echo hi >&3' 3>&1 | cat\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The incoming pipe is a different pipe, and a copy of stdin has to reach
    // that one rather than the outgoing one.
    let out = run_with_input("puts fed | sh -c 'cat <&3' 3<&0\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "fed\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Both at once, in one stage: fd 3 is the pipe feeding it and fd 4 the pipe
    // it feeds.
    let out = run_with_input("puts both | sh -c 'cat <&3 >&4' 3<&0 4>&1 | cat\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "both\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And through a function, which makes its own pipe rather than letting
    // `Command` make one.
    let out = run_with_input("func f() { sh -c 'echo deep >&3' }\nf 3>&1 | cat\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "deep\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A stage that also redirects stdout still owes the next stage the pipe,
    // since fd 3 took it before `> file` moved stdout away.
    let dir = fresh_dir("high_fd_pipe");
    let log = dir.join("log.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo piped >&3; echo filed' 3>&1 > {} | cat\n",
        log.to_string_lossy()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "piped\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "filed\n");

    // The same spelling backgrounded. A background stage defers its opens to a
    // helper that resolves the redirections itself, seeded with what the shell
    // handed it — so fd 3 copies the pipe only if the shell made one and put it
    // on the helper's stdout. Without it the stage wrote straight to the
    // terminal, skipping the rest of the pipeline.
    let deferred = dir.join("deferred.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo low >&3; echo filed' 3>&1 > {} | tr a-z A-Z &\nsleep 0.4\n",
        deferred.to_string_lossy()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "LOW\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&deferred).unwrap_or_default(),
        "filed\n"
    );
}

#[test]
fn an_eof_stdin_copied_to_a_high_descriptor_still_reads() {
    // A stage whose producer redirected its output away has no incoming pipe, so
    // stdin is `/dev/null`. Only stdin has a slot the shell fills with one, so a
    // descriptor that copied stdin was left closed and read `EBADF` rather than
    // end-of-file.
    let dir = fresh_dir("null_high_fd");
    let out = run_with_input(&format!(
        "echo hi > {} | sh -c 'cat <&3; echo done'\n",
        dir.join("sink.txt").to_string_lossy()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "done\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_with_input(&format!(
        "echo hi > {} | sh -c 'cat <&3; echo done' 3<&0\n",
        dir.join("copied.txt").to_string_lossy()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The `echo done` runs either way, so the read is what has to be checked.
    assert!(!stderr.contains("Bad file descriptor"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n", "{stderr}");
}

/// Run one command in a mesh whose own `RLIMIT_NOFILE` is `limit`, so a
/// redirection can be pushed past the descriptors the process may hold. In `-c`
/// mode mesh holds only the standard three, so the next free descriptor is 3.
fn run_at_descriptor_limit(limit: libc::rlim_t, command_line: &str) -> Output {
    use std::os::unix::process::CommandExt as _;

    let mut command = mesh_command();
    command.arg("-c").arg(command_line);
    // SAFETY: `setrlimit` only lowers this child's own descriptor limit, and is
    // async-signal-safe, which is the bar `pre_exec` sets.
    unsafe {
        command.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // At this limit the child can afford 0, 1, 2 and almost nothing else, so it
    // has to start with exactly those. A descriptor another test had open when
    // this process forked is inherited here, and one landing on the single free
    // slot leaves the dynamic loader unable to open a shared library: the
    // process dies before `main` with `error while loading shared libraries: …
    // Error 24` — `EMFILE` — which arrives looking like the shell refusing a
    // redirection it never got to see.
    //
    // `open_pty_pair` closes the usual source of those descriptors, but not the
    // gap between `openpty` returning and the flag being set on its results.
    // That gap belongs to another test's fork and is not this one's to schedule,
    // so a run that lands in it is retried rather than reported as a result
    // about redirection.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let out = command.output().expect("run mesh");
        let died_before_main =
            String::from_utf8_lossy(&out.stderr).contains("error while loading shared libraries");
        if !died_before_main || std::time::Instant::now() > deadline {
            break out;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn a_redirection_needs_no_descriptor_to_spare() {
    // Installing a descriptor must not require a *free* one above it. Lifting
    // every handle clear of every target is collision-safe and simple, but it
    // asks for headroom `RLIMIT_NOFILE` can refuse: at a limit of 4 the only
    // descriptors that exist are 0-3, so `3> file` had nowhere to go and failed
    // with `Invalid argument` on a redirection bash performs happily.
    let dir = fresh_dir("fd_limit");
    let sink = dir.join("out.txt");
    let out = run_at_descriptor_limit(4, &format!("puts ok 3> {}", sink.to_string_lossy()));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Past the limit there is no descriptor to install onto, and that is refused
    // before *this* redirection's own target is opened — named the way bash
    // names it, rather than surfacing later as `EBADF` against the command.
    let out = run_with_input(&format!(
        "echo hi {}> {}\nputs after\n",
        libc::c_int::MAX,
        dir.join("never.txt").to_string_lossy()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "the shell died on it: {stderr}"
    );
    assert!(stderr.contains(&libc::c_int::MAX.to_string()), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        !dir.join("never.txt").exists(),
        "the target was created for a redirection that cannot work"
    );
}

#[test]
fn a_descriptor_can_be_closed_for_a_command() {
    // `n>&-` takes the descriptor away rather than pointing it anywhere. Every
    // route has to honour it, since each installs descriptors its own way — and
    // the in-shell one must put the shell's own descriptor back afterwards, the
    // way it does for a redirected one.
    let dir = fresh_dir("close_descriptor");
    let both = "sh -c 'echo out; echo err >&2'";
    // `err` is the marker the closed descriptor would have carried, so look for
    // it as a whole line. A bare `contains` also matches the "err" inside the
    // "error" of any unrelated diagnostic, which reports someone else's bug
    // under this test's name and message.
    let leaked = |stderr: &[u8]| String::from_utf8_lossy(stderr).lines().any(|l| l == "err");

    let out = run_with_input(&format!("{both} 2>&-\n"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert!(
        !leaked(&out.stderr),
        "stderr survived the close: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A function, whose redirections apply to the shell's own descriptors.
    let out = run_with_input(&format!(
        "func f() {{ {both} }}\nf 2>&-\nsh -c 'echo back >&2'\n"
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!leaked(&out.stderr), "{stderr}");
    assert!(
        stderr.contains("back"),
        "fd 2 stayed closed after the redirection: {stderr}"
    );

    // A pipeline stage and a background command, the two routes that install
    // descriptors somewhere other than the spawning shell.
    let out = run_with_input(&format!("{both} 2>&- | cat\n"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert!(!leaked(&out.stderr));

    let sink = dir.join("bg.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo out > {}; echo err >&2' 2>&- &\nsleep 0.4\n",
        sink.to_string_lossy()
    ));
    assert_eq!(std::fs::read_to_string(&sink).unwrap_or_default(), "out\n");
    assert!(
        !leaked(&out.stderr),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A backgrounded builtin or function opens its targets in its own fork, so
    // the closes have to travel with them — that path converted the resolved
    // destinations to files and then passed no closes at all, and `puts` wrote
    // to a descriptor the redirection had taken away.
    let out = run_with_input("puts leaked 1>&- &\nsleep 0.4\n");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("leaked"),
        "the close did not reach the backgrounded stage: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Closing needs no descriptor, so the process limit has nothing to say about
    // it: this asks for something already true, and bash accepts it.
    let out = run_with_input("true 999999>&-\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Source order decides here too: a descriptor closed earlier is gone, so
    // copying it afterwards is `EBADF` — the same answer bash gives.
    let out = run_with_input("sh -c 'echo x' 3>&- 4>&3\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // And it decides *over* what the shell happens to hold. With an enclosing
    // `f 3> out` the shell really does have a descriptor 3, and asking whether
    // it does let the copy reach past the close to that file — the command ran,
    // where bash refuses it. What the walk itself did to fd 3 settles it.
    let out = run_with_input(&format!(
        "cd {}\nfunc f() {{ sh -c 'echo RAN' 3>&- 4>&3 }}\nf 3> enclosing\nputs after\n",
        dir.display()
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Bad file descriptor"), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after\n",
        "the copy reached past the close: {stderr}"
    );

    // The copy that has no close before it still works, which is the rule this
    // must not erode: the shell's own fd 3 is a real descriptor to copy.
    let out = run_with_input(&format!(
        "cd {}\nfunc g() {{ sh -c 'echo nested >&4' 4>&3 }}\ng 3> inherited\n",
        dir.display()
    ));
    assert_eq!(
        std::fs::read_to_string(dir.join("inherited")).unwrap_or_default(),
        "nested\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_descriptor_limit_is_checked_in_source_order() {
    // The check used to be a pre-pass over the whole list, so a redirection the
    // process cannot hold cancelled the ones *before* it as well. Source order
    // says the earlier ones have already happened: at a limit of 4, `> existing`
    // truncates and only then is `4>` refused.
    //
    // bash goes one step further and creates `later` too — it opens the target
    // and fails on the `dup2` — so mesh stays the less destructive of the two by
    // refusing a descriptor it could never install onto before creating a file
    // for it. That difference is deliberate; the ordering one was not.
    let dir = fresh_dir("fd_limit_order");
    let existing = dir.join("existing.txt");
    let later = dir.join("later.txt");
    std::fs::write(&existing, "keepme\n").expect("seed the file");
    let out = run_at_descriptor_limit(
        4,
        &format!(
            "puts hi > {} 4> {}",
            existing.to_string_lossy(),
            later.to_string_lossy()
        ),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("&4"),
        "the failure did not name it: {stderr}"
    );
    assert!(stderr.contains("Bad file descriptor"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap_or_default(),
        "",
        "the redirection before the refused one was skipped"
    );
    assert!(
        !later.exists(),
        "a target was created for a descriptor that cannot be installed onto"
    );
}

#[test]
fn a_duplication_that_cannot_be_afforded_stops_the_walk() {
    // A duplication takes a descriptor, and taking it can fail — `EMFILE` at the
    // limit. Proving it well-formed during the walk but performing it afterwards
    // meant a later `>` had already truncated by the time the failure surfaced.
    //
    // At a limit of 5 the standard three leave two: `3> foo` takes one, `4>&3`
    // takes the other, and `> existing` is the one that cannot be afforded — so
    // `existing` keeps its contents, exactly as bash leaves it.
    let dir = fresh_dir("fd_limit_dup");
    let foo = dir.join("foo.txt");
    let existing = dir.join("existing.txt");
    std::fs::write(&existing, "keepme\n").expect("seed the file");
    let out = run_at_descriptor_limit(
        5,
        &format!(
            "puts hi 3> {} 4>&3 > {}",
            foo.to_string_lossy(),
            existing.to_string_lossy()
        ),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Too many open files"),
        "expected the descriptor limit to be reached: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap_or_default(),
        "keepme\n",
        "the unaffordable duplication truncated the target after it"
    );
}

#[test]
fn duplicating_an_unopened_descriptor_is_an_error() {
    // A copy of nothing is not an inheritance of the shell's own descriptor of
    // that number, so this is `EBADF` — the answer the kernel itself gives.
    let out = run_with_input("cat 2>&7\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // Order still decides, as it does for the standard three: the duplication
    // reads the descriptor as it stands at that point, so opening it afterwards
    // is too late.
    let dir = fresh_dir("late_descriptor");
    let log = dir.join("log.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo x 1>&2' 2>&3 3> {}\nputs after\n",
        log.to_string_lossy()
    ));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(
        !log.exists(),
        "a target after the failed duplication was opened anyway"
    );

    // And source order means the failure stops the sequence *there*: a later
    // `>` must not have already emptied an existing file by the time the bad
    // duplication is reported. bash leaves it alone; opening every target up
    // front and only then resolving did not.
    let kept = dir.join("kept.txt");
    std::fs::write(&kept, "keepme\n").expect("seed the file");
    let out = run_with_input(&format!("true 2>&7 > {}\n", kept.to_string_lossy()));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&kept).unwrap_or_default(),
        "keepme\n",
        "the failed redirection truncated the later target"
    );
}

#[test]
fn a_duplication_can_copy_a_descriptor_the_shell_already_holds() {
    // The seed carries only the standard three, so a copy of anything higher
    // read as a copy of nothing even when the shell genuinely held it — and an
    // enclosing redirection is exactly how a high descriptor comes to be held.
    // bash writes `nested`; mesh reported `&3: Bad file descriptor`.
    let dir = fresh_dir("inherited_descriptor");
    let out = run_with_input(&format!(
        "func f() {{ sh -c 'echo nested >&4' 4>&3 }}\nf 3> {}\n",
        dir.join("out.txt").to_string_lossy()
    ));
    assert_eq!(
        std::fs::read_to_string(dir.join("out.txt")).unwrap_or_default(),
        "nested\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Still `EBADF` when nothing holds it, which is the rule this must not
    // erode: the fallback asks the process, not a guess.
    let out = run_with_input("sh -c 'echo x >&4' 4>&3\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Bad file descriptor"),
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
    // `wait` for the reader rather than sleeping at it. The shell hangs up its
    // jobs on the way out, so an exit that beat the `cat` killed the very job
    // whose output is being asserted on — 50ms was the whole margin, and the
    // failure is a missing "payload" rather than a late one.
    let out = run_with_input(&format!(
        "cd {}\ncat < f &\nputs ready\necho payload > f\nwait 1\n",
        dir.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ready\npayload\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_redirect_does_not_require_sh_on_path() {
    // A background stage opens its own targets after the fork, which must not
    // cost a wrapper executable to be found: nothing here is reachable through
    // `PATH` except the command the user actually named.
    let dir = fresh_dir("background_redirect_path");
    let output = dir.join("out");
    let mut child = mesh_command()
        .env("PATH", "/definitely-missing")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    // Wait for the job's own output rather than guessing at how long it needs.
    // A fixed sleep raced it two ways: the shell hangs up its jobs on the way
    // out, so an exit that beat the job killed it, and the redirect creates the
    // file before the command writes — so a slow `/bin/echo` left an empty file
    // rather than a missing one, and the assertion read back "".
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "/bin/echo ok > {} &", output.display()).expect("write commands");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read_to_string(&output).unwrap_or_default() != "ok\n" {
        assert!(
            std::time::Instant::now() < deadline,
            "the backgrounded redirect never wrote its output"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    writeln!(stdin, "jobs").expect("write jobs");
    drop(stdin);
    let result = child.wait_with_output().expect("wait for mesh");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "ok\n");
    assert!(!String::from_utf8_lossy(&result.stderr).contains("command not found"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_background_redirect_reports_mesh_status_one() {
    let dir = fresh_dir("background_redirect_failure");
    let missing = dir.join("missing/out");
    // The stage reports its own failure, from a process writing to the same
    // stderr as the shell's job notices, so assert on whole *lines*: a
    // non-atomic write splices the two together (`mesh: [1] 4242` + an orphaned
    // remainder) and a plain `contains` on a prefix would still pass. Repeat the
    // run because the splice needs the two writers to overlap, which only
    // happens under contention.
    //
    // The script waits on the job's own state rather than for a fixed interval:
    // `jobs` has to run once the job is reapable or there is no `Done` line to
    // match at all, and `/bin/sleep 0.05` lost that race under load — leaving a
    // stderr with the launch notice and the helper's error but no `Done (1)`.
    // `wait` is the wrong tool *here* despite being the right one elsewhere: it
    // takes the job out of the table, so the `Done (1)` notice this asserts on
    // would never be printed. The launch notice and the helper's error still
    // race each other, so the contention the splice needs is untouched.
    for _ in 0..5 {
        let out = run_with_input(&format!(
            "/bin/echo ok > {} &\nwhile $sh.jobs[1].state == running {{ /bin/sleep 0.02 }}\njobs\n",
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
    let out = run_with_input("cd --help\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Change the working directory\n\nUsage: cd [DIR]\n\nOptions:\n  --help  Print help\n"
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn help_lists_every_builtin_with_its_usage() {
    let out = run_with_input("help\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for usage in [
        "cd [DIR]",
        "puts [ARG ...]",
        "print [ARG ...]",
        "clip [TEXT ...]",
        "notify [TEXT ...]",
        "exit [N]",
        "fg [JOB]",
        "bg [JOB]",
        "jobs",
        "wait [JOB …]",
        "disown [-h] [-a | -r] [JOB …]",
        "kill [-SIGNAL] JOB|PID ...",
        "prompt [--reset | TEXT]",
        "on [--remove] EVENT NAME [FUNCTION]",
        "source FILE",
        "help [NAME ...]",
    ] {
        assert!(stdout.contains(&format!("  {usage}")), "{usage}: {stdout}");
    }
    // The summary is what tells a reader which builtin they want.
    assert!(
        stdout.contains("Copy text to the terminal's clipboard"),
        "{stdout}"
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn help_lists_the_keywords_and_the_shape_of_a_line() {
    let out = run_with_input("help\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for form in [
        "cmd | cmd",
        "cmd && cmd",
        "cmd > FILE",
        "cmd &",
        "NAME = VALUE",
        "global NAME = VALUE",
        "export NAME = VALUE",
        "unset NAME",
        "if COND { … } else { … }",
        "cmd if COND",
        "match VALUE { PAT => … ; … }",
        "for NAME in VALUE { … }",
        "while COND { … }",
        "loop { … }",
        "break",
        "func NAME(PARAMS) { … }",
        "return [VALUE]",
        "fork { … }",
        "cmd << END … END",
        "[a b]  [key: value]",
        "...$xs",
        "$path:base",
        "$x == $y",
        "n = (1 + 2)",
        "$name ~ *.txt",
    ] {
        assert!(stdout.contains(&format!("  {form}")), "{form}: {stdout}");
    }
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn help_explains_every_keyword_and_operator_a_line_can_carry() {
    // A reader asks with the word or symbol they just typed, so each has to
    // answer — `unless`, `+=`, and `==` are as much syntax as `if` and `|` are.
    let script: String = KEYWORDS_AND_OPERATORS
        .iter()
        .map(|name| format!("help '{name}'\n"))
        .collect();
    let out = run_with_input(&script);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "a keyword or operator went unexplained"
    );
    assert!(out.status.success());
}

/// Everything the parser reserves or reads as an operator. Held here as well as
/// in the unit tests so the promise is checked through the shell a reader types
/// into, not only through the table behind it.
const KEYWORDS_AND_OPERATORS: &[&str] = &[
    "func", "return", "if", "else", "unless", "match", "for", "in", "while", "loop", "break",
    "continue", "fork", "global", "unset", "export", "not", "and", "or", "re", "|", "|&", "&&",
    "||", ";", "&", ">", "<", ">>", "2>", ">&", "<&", "&>", "<<", "<<<", "=", "+=", "==", "!=",
    "<=", ">=", "+", "-", "/", "%", "*", "?", "~", "!~", "$", "$(", "...", ":", ".", ",", "(", ")",
    "[", "]", "{", "}", "..", "..=", "=>",
];

#[test]
fn help_explains_a_keyword_by_its_syntax() {
    let out = run_with_input("help for\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Repeat over a list, a range, or a map\n\nSyntax: for NAME in VALUE { … }\n"
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn help_answers_for_the_other_half_of_a_construct() {
    // `help else` is a question about `if`; a keyword takes no `--help` of its
    // own, so `help` is the only way to ask.
    let out = run_with_input("help else\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Run a body when a condition holds\n\nSyntax: if COND { … } else { … }\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_with_a_name_prints_what_that_builtins_help_flag_prints() {
    let named = run_with_input("help cd\n");
    let flag = run_with_input("cd --help\n");
    assert_eq!(named.stdout, flag.stdout);
    assert!(named.status.success());
    assert!(named.stderr.is_empty());
}

#[test]
fn help_prints_every_name_it_was_given() {
    let out = run_with_input("help pwd jobs\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Print the working directory\n\nUsage: pwd\n\nOptions:\n  --help  Print help\n\
         \nList the jobs\n\nUsage: jobs\n\nOptions:\n  --help  Print help\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_reports_a_name_that_is_not_a_builtin_and_keeps_the_rest() {
    // An external command's help is its own; a typo must not cost the names
    // beside it either.
    let out = run_with_input("help nosuchbuiltin pwd\nputs \"status $sh.status\"\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage: pwd"), "{stdout}");
    assert!(stdout.contains("status 1"), "{stdout}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("mesh: help: nosuchbuiltin: not a builtin or a keyword"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn help_is_a_builtin_a_pipeline_can_read() {
    let out = run_with_input("help | grep -c 'Print the working directory'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn type_names_a_function_by_its_signature() {
    let out = run_with_input("func ll(...args) { ls -l ...$args }\ntype ll\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ll is a function\n    func ll(...args)\n"
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn type_shows_a_functions_flags_and_optionals_in_its_signature() {
    let out = run_with_input(
        "func deploy(target, --region = us-west, --force, ...hosts) { puts hi }\ntype deploy\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "deploy is a function\n    func deploy(target, --region = …, --force, ...hosts)\n"
    );
}

#[test]
fn type_names_a_builtin_and_a_keyword() {
    // `on` rather than `pwd`, which is also an external on most systems
    // and so answers with a shadow note that has nothing to do with this.
    let out = run_with_input("type on\ntype unless\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("on is a shell builtin\n    on [--remove]"),
        "{stdout}"
    );
    assert!(
        stdout.contains("unless is a shell keyword\n    cmd if COND"),
        "{stdout}"
    );
}

#[test]
fn type_finds_an_external_on_path() {
    let out = run_with_input("type sh\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("sh is /"), "{stdout}");
    assert!(stdout.contains("/sh"), "{stdout}");
}

#[test]
fn type_names_only_the_winner_until_a_is_asked_for() {
    // Bash's shape: bare `type` reports the winner and says nothing about what it
    // displaced; `-a` is where every match belongs. Describing what a name could
    // have matched but did not is not worth a line of its own.
    let out = run_with_input("func sh(...args) { puts nope }\ntype sh\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("sh is a function\n"), "{stdout}");
    assert!(!stdout.contains("shadowing"), "{stdout}");
    assert!(stdout.contains("\n    func sh(...args)\n"), "{stdout}");
    // `-a` lists the function and the external it displaced, in resolution order.
    let out = run_with_input("func sh(...args) { puts nope }\ntype -a sh\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("sh is a function\n"), "{stdout}");
    assert!(stdout.contains("\nsh is /"), "{stdout}");
}

/// A builtin's name is reserved against `func`, so taking `type` costs the ability
/// to define one — as `whence` cost it before. The rename pointers are *not*
/// reserved, so a reader who wants their own `what` or `where` still has it.
#[test]
fn type_is_reserved_against_func_but_the_pointers_are_not() {
    let out = run_with_input("func type(x) { puts mine }\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`type` is a reserved name"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `whence`, `what` and `where` only point at `type`; a function may take them.
    let out = run_with_input(
        "func what(x) { puts \"mine $x\" }\n\
         func where(x) { puts \"mine $x\" }\n\
         func whence(x) { puts \"mine $x\" }\n\
         what a\nwhere b\nwhence c\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "mine a\nmine b\nmine c\n"
    );
}

/// Deeply nested input is refused, not fatal. Every one of these aborted the whole
/// shell with `thread 'main' has overflowed its stack` — a `SIGABRT`, no output, no
/// diagnostic — which turns malformed input into a dead shell. The shapes descend
/// by two different paths: a group, a list and a capture reach `primary`, while a
/// statement-position `if` does not and needs `block` counted separately.
#[test]
fn nesting_past_the_limit_is_an_error_not_an_abort() {
    for source in [
        format!("x = {}1{}\n", "(".repeat(5000), ")".repeat(5000)),
        format!("x = {}1{}\n", "[".repeat(5000), "]".repeat(5000)),
        format!("puts {}pwd{}\n", "$(".repeat(2000), ")".repeat(2000)),
        format!("{}puts x{}\n", "if true { ".repeat(2000), " }".repeat(2000)),
        format!("x = {}1{}\n", "if true { ".repeat(2000), " }".repeat(2000)),
        // A capture inside a string is lexed where it is found, so this one recurses
        // through the *lexer* and needs its own counter — the parser never sees it.
        format!("puts {}x{}\n", "\"$(puts ".repeat(2000), ")\"".repeat(2000)),
        // Alternating the two paths, which is what says they share one budget rather
        // than each having a full one.
        format!(
            "puts {}1{}\n",
            "\"$(puts ((".repeat(1000),
            "))\")\"".repeat(1000)
        ),
        // The shapes below recurse *after* `primary` has given its level back, so
        // each needs counting where it descends rather than on the way in. Every
        // one of them exhausted the stack while the three counters above were in
        // place, which is the whole reason they are listed separately.
        //
        // An `else if` chain: the preceding block has returned before the tail
        // recurses, so `block`'s counter is already back to where it started.
        format!(
            "if false {{ puts a }} {}\n",
            "else if false { puts x } ".repeat(5000)
        ),
        // Prefix chains, which recurse in `prefix` on the way *to* `primary`.
        format!("x = {}1\n", "- ".repeat(20000)),
        format!("x = {}[1]\n", "...".repeat(20000)),
        // The trailer loop's own descents — call arguments, an index expression,
        // and a modifier's arguments all reparse from the top.
        format!("x = {}1{}\n", "f(".repeat(3000), ")".repeat(3000)),
        format!("x = $a{}[0]{}\n", "[$a".repeat(3000), "]".repeat(3000)),
        format!("x = $x{}$x{}\n", ":upper(".repeat(3000), ")".repeat(3000)),
    ] {
        let out = run_with_input(&source);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("nested too deeply"), "{stderr}");
        // A parse error, not a crash: `134` is the abort this replaces.
        assert_eq!(out.status.code(), Some(2), "{stderr}");
    }
}

/// The limit is only worth having if it is *reachable* — if the stack runs out
/// first it is decoration, and the abort it was meant to replace happens anyway.
/// So this parses right up to the ceiling as well as at ordinary depths.
#[test]
fn nesting_within_the_limit_still_parses() {
    let out = run_with_input(
        "x = ((((1 + 2))))\n\
         puts $x\n\
         puts [[1 2] [3 [4 5]]]\n\
         y = if true { if false { 1 } else { if true { 2 } else { 3 } } } else { 4 }\n\
         puts $y\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3\n[[1 2] [3 [4 5]]]\n2\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Ordinary nesting of a capture in a string is untouched by the lexer's counter.
    let out = run_with_input("puts \"a$(puts \"b$(puts c)b\")a\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "abcba\n");

    // One level under the ceiling, for the most expensive shape the parser has.
    // A capture costs the most stack per level, so if anything reaches the limit
    // by running out of stack first it is this.
    let source = format!("puts {}pwd{}\n", "$(".repeat(99), ")".repeat(99));
    let out = run_with_input(&source);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let source = format!("x = {}1{}\nputs $x\n", "(".repeat(99), ")".repeat(99));
    let out = run_with_input(&source);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");

    // A trailer that descends is counted where it descends, so a nesting of calls
    // costs one level each and not two. Counting the trailer loop as a whole would
    // have charged every operand a second level and quietly halved the limit; this
    // is what would catch that. It parses — the failure is at run time, about the
    // undefined `f`, which is proof enough that the parse got through.
    let source = format!("x = {}1{}\n", "f(".repeat(99), ")".repeat(99));
    let out = run_with_input(&source);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("nested too deeply"), "{stderr}");

    // Chains that do *not* descend stay free: an index chain reparses nothing, so
    // its length is not nesting and must not be charged as if it were.
    let out = run_with_input(&format!(
        "a = [[1 2] [3 4]]\nputs $a[0]{}\n",
        "[0]".repeat(97)
    ));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("nested too deeply"), "{stderr}");
}

/// The last resort, for the recursion no parse-time limit can see.
///
/// `1 + 1 + …` parses *iteratively* into a left-leaning spine, so it is never
/// deep at parse time and the nesting limit never fires — it is the evaluator
/// walking that spine that runs out of stack. Before the fault handler this was
/// a `SIGABRT` with Rust's own `has overflowed its stack` on stderr: nothing a
/// script could test, and an interactive session that simply vanished.
///
/// This is deliberately not a *recovery*. The shell still exits; what changed is
/// that it says why and leaves a status behind. Fixing the evaluator so the case
/// does not arise is tracked separately in `TODO.md`.
#[test]
fn running_out_of_stack_reports_instead_of_aborting() {
    let source = format!("x = {}1\nputs unreachable\n", "1 + ".repeat(20000));
    let out = run_with_input(&source);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mesh: fatal: out of stack"), "{stderr}");
    // Distinct from the `2` a syntax error uses: the input was well formed and it
    // was the shell that could not continue. `134` is the abort this replaces.
    assert_eq!(out.status.code(), Some(70), "{stderr}");
    // Nothing after the failure ran.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// The other case the nesting limit cannot cover: a stack too small for the limit
/// to be reachable at all.
///
/// Under a small `ulimit -s` the parser runs out of stack *below* `MAX_DEPTH`, so
/// the check never fires — the limit is sized for the stack a shell normally
/// starts with, and this is the shape of input that gets under it. The handler is
/// the whole reason this is a message rather than the abort it used to be.
#[test]
fn a_stack_too_small_for_the_limit_still_reports() {
    let source = format!("puts {}pwd{}\n", "$(".repeat(90), ")".repeat(90));
    let out = run_with_input_and_stack_limit(&source, 512 * 1024);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mesh: fatal: out of stack"),
        "expected a diagnostic, got {stderr}"
    );
    assert_eq!(out.status.code(), Some(70), "{stderr}");
    // Specifically not Rust's abort, which is what this replaces.
    assert!(!stderr.contains("has overflowed its stack"), "{stderr}");
}

/// `-t` prints bash's one word, because this output is *compared* rather than read:
/// a port that carries over `case "$(type -t "$1")" in function)` keeps working,
/// where matching prose against the sentence breaks the moment the wording moves.
/// `variable` is the one addition — bash's `type` cannot see bindings.
#[test]
fn type_t_prints_one_bash_word_per_name() {
    let out = run_with_input(
        "func ll(...args) { ls }\n\
         xs = [a b]\n\
         type -t ll\ntype -t cd\ntype -t ls\ntype -t if\ntype -t xs\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "function\nbuiltin\nfile\nkeyword\nvariable\n"
    );
    // Nothing printed and a failing status when there is no answer, as bash does.
    let out = run_with_input("type -t definitely-no-such-command\n");
    assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    assert!(!out.status.success());
}

/// `-P` answers only with a `PATH` hit, ignoring functions and builtins. This is
/// what retires the hand-rolled `for d in $PATH` loop a portable `shrc` carries,
/// since `type -P` is not available everywhere.
#[test]
fn type_p_prints_only_a_path_hit() {
    let out = run_with_input("func sh(...args) { puts nope }\ntype -P sh\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with('/'), "{stdout}");
    assert!(!stdout.contains("function"), "{stdout}");
    // A function or a builtin has no path, which is the whole point of the flag.
    for name in ["ll", "cd"] {
        let out = run_with_input(&format!("func ll(...a) {{ ls }}\ntype -P {name}\n"));
        assert!(out.stdout.is_empty(), "{name}: {:?}", out.stdout);
        assert!(!out.status.success(), "{name}");
    }
}

#[test]
fn type_all_lists_every_match_in_resolution_order() {
    let out = run_with_input("func sh(...args) { puts nope }\ntype -a sh\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("sh is a function\n    func sh(...args)\nsh is /"),
        "{stdout}"
    );
    // The winner keeps its plain headline: `-a` is the listing, not a note.
    assert!(!stdout.contains("shadowing"), "{stdout}");
}

#[test]
fn type_reports_a_binding_by_its_bare_name() {
    // A variable is asked about **without** the sigil: `$xs` would expand before
    // `type` ever saw it, and the value it produced could not say where it came
    // from. The name is the question.
    let out = run_with_input("xs = [a b c]\ngreeting = hi\ntype xs greeting\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "xs is a variable\n    a list of 3: ['a', 'b', 'c']\n\
         greeting is a variable\n    a string: 'hi'\n"
    );
}

#[test]
fn type_names_the_scope_a_binding_lives_in() {
    let out = run_with_input(
        "global outer = 1\nfunc peek() { inner = 2; type inner; type outer }\npeek\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "inner is a local variable\n    an integer: 2\n\
         outer is a variable\n    an integer: 1\n"
    );
}

#[test]
fn type_reads_the_environment_as_its_own_namespace() {
    // A command and an environment entry are separate namespaces in mesh, so a
    // name that is both is reported as both rather than one winning.
    let out = run_with_input("export MESH_WHENCE_E2E = hi\ntype MESH_WHENCE_E2E\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "MESH_WHENCE_E2E is an environment entry\n    a string: 'hi'\n"
    );
}

#[test]
fn type_reports_a_path_operand_rather_than_searching_for_it() {
    let out = run_with_input("type ./Cargo.toml\ntype /\n");
    // `/` is division first — the parser settles a shape before any lookup — and
    // the directory it also names comes after, neither one shadowing the other.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "./Cargo.toml is a file, but not executable\n\
         / is a shell keyword\n    n = (1 + 2)\n/ is a directory\n"
    );
}

#[test]
fn type_quiet_is_the_command_v_test() {
    // `--quiet` makes the status the whole answer: no report on stdout, and no
    // not-found note on stderr either, so a startup-file test stays silent
    // without redirecting two streams.
    let out = run_with_input(
        "if type --quiet sh { puts found }\nif type --quiet definitely-no-such-command { puts bad } else { puts missing }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "found\nmissing\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn type_prints_its_report_when_it_is_a_condition() {
    // Without `--quiet` it is an ordinary command that writes: the status is
    // still the test, but the report goes to stdout as it always does.
    let out = run_with_input("if type sh { puts found }\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("sh is /"), "{stdout}");
    assert!(stdout.ends_with("found\n"), "{stdout}");
}

#[test]
fn type_reports_a_missing_name_and_keeps_the_rest() {
    let out = run_with_input("type nosuchname pwd\nputs \"status $sh.status\"\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("pwd is a shell builtin"), "{stdout}");
    // A name that resolved still prints, so one typo does not cost the rest.
    assert!(stdout.contains("status 1"), "{stdout}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("mesh: type: nosuchname: not found"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn type_points_another_shells_spelling_at_mesh_s() {
    // Both directions: typing another shell's name, and asking `type` about it.
    // `whence` is ksh's, `what` is nobody's but is the name a reader may have
    // carried in from their own config. `which` is deliberately absent — it is a
    // real program on disk, and mesh leaves it alone.
    let out = run_with_input("whence ls\nwhat ls\ntype whence\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mesh: command not found: whence (mesh spells this `type`)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("mesh: command not found: what (mesh spells this `type`)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("mesh: type: whence: not found (mesh spells this `type`)"),
        "{stderr}"
    );
}

#[test]
fn type_terminator_asks_about_a_flag_looking_name() {
    let out = run_with_input("type -- --all\ntype --nope\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mesh: type: --all: not found"), "{stderr}");
    assert!(
        stderr.contains("mesh: type: --nope: unknown option (`-t`, `-P`, `-a` or `--quiet`)"),
        "{stderr}"
    );
}

#[test]
fn type_is_a_builtin_a_pipeline_can_read() {
    let out = run_with_input("type pwd | grep -c builtin\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn type_fails_for_a_shape_the_parser_does_not_claim() {
    // `help`'s table documents an operator for every symbol a line can carry, and
    // words the parser claims either always or only *contextually*. Only what it
    // claims in command position runs when typed bare, so only that may report
    // success — `unless` alone is `command not found`, for all that `cmd if COND`
    // is real syntax, while `if` is claimed unconditionally as the prefix
    // conditional and so does resolve.
    let out = run_with_input(
        "type +\nputs \"plus $sh.status\"\n\
         type unless\nputs \"unless $sh.status\"\n\
         type if\nputs \"if $sh.status\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "+ is a shell keyword\n    n = (1 + 2)\nplus 1\n\
         unless is a shell keyword\n    cmd if COND\nunless 1\n\
         if is a shell keyword\n    if COND { … } else { … }\nif 0\n"
    );
}

#[test]
fn type_lets_a_function_outrank_a_contextual_keyword() {
    // `fork` is the subshell keyword only before a block, so `func fork()` is legal
    // and a bare `fork` calls it — see `a_command_named_fork_is_still_reachable`.
    // The function is therefore the answer to "what runs", and the keyword is
    // reported beside it rather than shadowing it.
    let out = run_with_input("func fork() { puts CALLED }\ntype fork\nfork\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "fork is a function\n    func fork()\n\
         fork is a shell keyword\n    fork { … }\n\
         CALLED\n"
    );
    // No shadow note in either direction: they are not competing for the position.
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("shadowing"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn type_names_a_job_handle_as_a_job() {
    // Diagnostics group jobs with streams — both lack a byte form — but the
    // question here is what the name holds, and `j = cmd &` holds a job.
    let out = run_with_input("j = sleep 0 &\ntype j\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("j is a variable\n    a job handle\n"),
        "{stdout}"
    );
}

#[test]
fn type_refuses_an_execute_bit_on_a_special_file() {
    // `execve` refuses a fifo whatever its mode, so the bit alone must not make
    // one look runnable — and "a named pipe" says why better than "not
    // executable" would.
    let dir = fresh_dir("type_fifo");
    let fifo = dir.join("p");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("path");
    // SAFETY: an ordinary `mkfifo(3)` call on a path this test owns.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o755) }, 0);
    let out = run_with_input(&format!(
        "type {0}\nputs \"status $sh.status\"\n",
        fifo.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{} is a named pipe\nstatus 1\n", fifo.display())
    );
}

#[test]
fn type_fails_for_a_path_operand_that_could_not_run() {
    // Both still print — knowing *why* a path is not a command is the useful
    // answer — but neither resolved, because running either is a `126`. Quiet
    // mode is the `command -v` test, so it must not say yes to them.
    let out = run_with_input(
        "type ./Cargo.toml\nputs \"file $sh.status\"\ntype /etc\nputs \"dir $sh.status\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "./Cargo.toml is a file, but not executable\nfile 1\n\
         /etc is a directory\ndir 1\n"
    );
    // An *executable* path operand still resolves — the rule is "could this
    // run", not "is it a path".
    let quiet = run_with_input(
        "if type --quiet ./Cargo.toml { puts bad } else { puts refused }\n\
         if type --quiet /bin/sh { puts runnable } else { puts bad }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&quiet.stdout),
        "refused\nrunnable\n"
    );
}

#[test]
fn type_searches_the_default_path_when_path_is_unset() {
    // `execvp` falls back to `confstr(_CS_PATH)` when `PATH` is unset, so a
    // sanitized environment still runs `sh` — and `type` has to still find it,
    // or it is wrong about the one thing it exists to be right about.
    let mut child = mesh_command()
        .env_remove("PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"type sh\n")
        .expect("write commands");
    let out = child.wait_with_output().expect("wait for mesh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("sh is /"), "{stdout}");
    assert!(stdout.contains("/sh"), "{stdout}");
    assert!(out.status.success(), "{:?}", out.status);
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
    // `two` never prints — `return` stops the body — but a returned *value* leaves
    // the status at 0, so `||` does not fire and `&&` does.
    let out = run_with_input("func f() { puts one; return 3; puts two }\nf && puts zero\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\nzero\n");
    // `fail` is the spelling that stops the body *and* reports a status.
    let failed = run_with_input("func f() { puts one; fail 3; puts two }\nf || puts nonzero\n");
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "one\nnonzero\n");
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
fn a_value_block_streams_its_commands_instead_of_capturing_them() {
    // The three value-producing blocks agree: output streams, and the block's value
    // is the last thing that *produced* one. Capturing here meant the same text
    // either streamed or was silently eaten depending on whether anyone bound the
    // result.
    let out = run_with_input(
        "a = if true { echo from-if }\n\
         b = match 1 { 1 => { echo from-match } }\n\
         func f() { echo from-func }\n\
         c = f()\n\
         puts \"<$a> <$b> <$c>\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "from-if\nfrom-match\nfrom-func\n<0> <0> <0>\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // A value expression in the tail still yields its value, in every one of them.
    let valued = run_with_input(
        "a = if true { \"x\" }\n\
         b = match 1 { 1 => { \"y\" } }\n\
         func f() { \"z\" }\n\
         c = f()\n\
         puts \"<$a> <$b> <$c>\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&valued.stdout), "<x> <y> <z>\n");
}

#[test]
fn a_failing_command_in_a_value_block_no_longer_skips_the_binding() {
    // The exit-0 gate that came with the capture failed *silently*: the assignment
    // was skipped entirely, so the error surfaced as an "unbound variable" on a
    // later line with nothing to say why. The status is now just the block's value.
    let out = run_with_input("x = if true { sh -c 'exit 3' }\nputs \"x=$x\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x=3\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn only_an_explicit_capture_takes_a_blocks_bytes() {
    // `$(…)` is the thing that means "capture", and it still does.
    let out = run_with_input("x = $(echo hi)\nputs \"<$x>\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<hi>\n");
}

#[test]
fn a_condition_is_a_bool_or_a_command_and_nothing_else() {
    // Every other type is refused by name, with the comparison to write instead.
    // Each of these used to branch, under a different rule per type.
    for (source, expected) in [
        (
            "if 0 { puts t } else { puts f }",
            "an int is not a condition",
        ),
        (
            "if \"0\" { puts t } else { puts f }",
            "a string is not a condition",
        ),
        (
            "if [] { puts t } else { puts f }",
            "a list is not a condition",
        ),
        (
            "if [:] { puts t } else { puts f }",
            "a map is not a condition",
        ),
    ] {
        let out = run_with_input(&format!("{source}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{source}: {stderr}");
        // Recoverable: the shell reports and carries on.
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{source}");
    }

    // A bool and a command are the two that work, and both still do.
    let ok = run_with_input(
        "if true { puts bool }\n\
         if 1 == 1 { puts comparison }\n\
         if sh -c 'exit 0' { puts command }\n\
         if sh -c 'exit 1' { puts wrong } else { puts command-failed }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&ok.stdout),
        "bool\ncomparison\ncommand\ncommand-failed\n"
    );
    assert!(ok.stderr.is_empty(), "{:?}", ok.stderr);
}

#[test]
fn a_length_in_condition_position_is_refused_rather_than_inverted() {
    // The footgun this rule exists for: `:len` returned an int, and an int read as
    // an exit status, so `if $xs:len` fired on the **empty** list and stayed quiet
    // on a full one. Both directions are now a diagnostic naming the comparison.
    let out = run_with_input("xs = []\nif $xs:len { puts has } else { puts empty }\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("an int is not a condition"), "{stderr}");
    assert!(
        stderr.contains(":len > 0") || stderr.contains("`… > 0`"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // Spelled as the comparison, both directions read the way they look.
    let compared = run_with_input(
        "xs = []\nys = [a b]\n\
         if $xs:len > 0 { puts wrong } else { puts empty }\n\
         if $ys:len > 0 { puts full } else { puts wrong }\n",
    );
    assert_eq!(String::from_utf8_lossy(&compared.stdout), "empty\nfull\n");
}

#[test]
fn and_or_and_not_refuse_the_same_values_a_condition_does() {
    // They ask the same question, so they refuse the same answers.
    for source in ["x = true and 1", "x = 0 or true", "x = not \"abc\""] {
        let out = run_with_input(&format!("{source}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("is not a condition"), "{source}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{source}");
    }

    // Short-circuiting still happens, and still yields a bool.
    let out = run_with_input(
        "puts (false and true)\nputs (true or false)\nputs (true and true)\nputs (not false)\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "false\ntrue\ntrue\ntrue\n"
    );
}

#[test]
fn fail_names_a_status_and_return_names_a_value() {
    // The two channels, spelled apart. `return` fills the value channel and leaves
    // the status at 0 — a result is success, whatever its type. `fail` fills the
    // status channel and leaves `false`, mesh's "no result", in the value channel.
    let out = run_with_input(
        "func v() { return 5 }\nfunc f() { fail 5 }\n\
         v\nputs \"return $sh.status\"\n\
         f\nputs \"fail $sh.status\"\n\
         x = v()\ny = f()\nputs \"[$x][$y]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "return 0\nfail 5\n[5][false]\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_bare_fail_is_status_one_and_zero_is_refused() {
    // Bare `fail` is the shell's ordinary "something went wrong".
    let bare = run_with_input("func f() { fail }\nf\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "1\n");

    // `fail 0` would be a `fail` that succeeded, which is always a mistake — the
    // spelling for leaving with success is `return true`.
    let zero = run_with_input("func f() { fail 0 }\nf\nputs after\n");
    let stderr = String::from_utf8_lossy(&zero.stderr);
    assert!(
        stderr.contains("fail: status must be between 1 and 255"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&zero.stdout), "after\n");

    // A non-integer operand is refused in the same terms.
    let word = run_with_input("func f() { fail nope }\nf\nputs after\n");
    let stderr = String::from_utf8_lossy(&word.stderr);
    assert!(
        stderr.contains("fail: status must be an integer"),
        "{stderr}"
    );
}

#[test]
fn a_bare_return_carries_the_last_status() {
    // "Stop here, as if the body ended at this line": no freshly minted status, and
    // a *failure* propagates just as readily as a success — the one place `return`
    // does not imply success.
    let out = run_with_input(
        "func after_ok() { sh -c 'exit 0'\n return }\n\
         func after_bad() { sh -c 'exit 3'\n return }\n\
         after_ok\nputs \"ok $sh.status\"\n\
         after_bad\nputs \"bad $sh.status\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok 0\nbad 3\n");
}

#[test]
fn a_predicate_function_still_reads_as_a_condition() {
    // `false` is the only value that fails, which is what keeps a predicate written
    // with `return true` / `return false` usable in command position.
    let out = run_with_input(
        "func yes() { return true }\nfunc no() { return false }\nfunc bad() { fail }\n\
         if yes { puts yes-taken }\n\
         if no { puts wrong } else { puts no-taken }\n\
         if bad { puts wrong } else { puts bad-taken }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "yes-taken\nno-taken\nbad-taken\n"
    );
}

#[test]
fn fail_is_a_reserved_function_name() {
    let out = run_with_input("func fail() { puts nope }\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("reserved name"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_return_reached_through_a_variable_is_typed_like_a_written_one() {
    // A written `return` is a control node whose operand is evaluated as a value,
    // so it carries any type. Reached through a variable (`r = return`) the word is
    // resolved after expansion instead, and that path used to build the result from
    // *argv* — which flattens. A list was refused outright ("list value needs
    // `...`") and a quoted `"42"` came back as the integer 42, so the same `return`
    // meant two things depending on how it was spelled.
    let list = run_with_input("func f() { xs = [a b c]\n r = return\n $r $xs }\nputs f():repr\n");
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        "['a', 'b', 'c']\n",
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(list.stderr.is_empty(), "{:?}", list.stderr);

    // Quoting separates the string from the integer here as it does everywhere
    // else a value is typed from a word.
    let quoted = run_with_input("func f() { r = return\n $r \"42\" }\nputs f():repr\n");
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "'42'\n");

    // A surplus operand keeps its answer.
    let surplus = run_with_input("func f() { r = return\n $r a b }\nf\n");
    assert!(
        String::from_utf8_lossy(&surplus.stderr).contains("return: too many arguments"),
        "{}",
        String::from_utf8_lossy(&surplus.stderr)
    );
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

    // In statement position the value is discarded and the statement *succeeds*:
    // an integer is a result, not an exit status, so 42 is status 0 exactly as
    // `41 + 1` is. Naming a status is `fail`'s job.
    let status = run_with_input("42\n");
    assert_eq!(status.status.code(), Some(0));
    assert!(status.stderr.is_empty(), "{:?}", status.stderr);
    assert_eq!(
        run_with_input("41 + 1\n").status.code(),
        status.status.code(),
        "a lone literal and the operator form should agree"
    );
}

#[test]
fn a_bare_word_is_a_command_and_a_quoted_one_is_a_string() {
    // The rule in one line: inside braces a bare word is a command, a quoted word
    // is a string literal. Before this, a *one-word* block tail was coerced to a
    // scalar — `{ pwd }` was the string "pwd" while `{ pwd . }` ran — so adding an
    // argument flipped a literal into an execution, and `x = if true { pwd }`
    // silently bound the wrong thing with no error to show for it.
    let out = run_with_input(
        "quoted = if true { \"pwd\" }\n\
         args = if true { echo hi }\n\
         puts \"<$quoted>\" \"<$args>\"\n",
    );
    // `pwd` and `echo hi` are commands, so they *run* and their output streams;
    // the block's value is the status they left, not their bytes. Only the quoted
    // word is a string literal.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi\n<pwd> <0>\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The carve-out: a numeral and a boolean can never name a command, so they stay
    // literals. This is what keeps `func answer() { 42 }` the integer.
    let literals = run_with_input(
        "func answer() { 42 }\nfunc no() { false }\n\
         puts answer():repr\nx = if true { false }\nputs $x:repr\n",
    );
    assert_eq!(String::from_utf8_lossy(&literals.stdout), "42\nfalse\n");

    // And a quoted word as a whole statement is a value, so it is what a bare
    // `return` carries — it no longer looks for a program of that name.
    let statement = run_with_input("func f() { \"foo\"\n return }\nputs f():repr\n");
    assert_eq!(String::from_utf8_lossy(&statement.stdout), "'foo'\n");
    assert!(statement.stderr.is_empty(), "{:?}", statement.stderr);
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
    //
    // The diagnostic comes from the stage's own process, after its redirections
    // have been applied, so `|&` carries it into the pipe and it arrives on
    // stdout — which is where bash puts it too.
    for (piped, reported_on_stdout) in [("42 | cat", false), ("42 |& cat", true)] {
        let out = run_with_input(&format!("{piped}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let reported = if reported_on_stdout { &stdout } else { &stderr };
        assert!(
            reported.contains("command not found: 42"),
            "{piped}: {reported:?}"
        );
        assert!(
            !stderr.contains("syntax error"),
            "{piped} must stay a pipeline: {stderr:?}"
        );
        assert!(stdout.ends_with("after\n"), "{piped}: {stdout:?}");
    }

    // The separators an expression statement *can* take are unaffected: 42 is a
    // value, so the statement succeeds — `&&` runs its right side and `||` does not.
    let and = run_with_input("42 && puts yes\nputs end\n");
    assert_eq!(String::from_utf8_lossy(&and.stdout), "yes\nend\n");
    let or = run_with_input("42 || puts no\nputs end\n");
    assert_eq!(String::from_utf8_lossy(&or.stdout), "end\n");

    // Two places where the classification *does* show through, both consistent with
    // rules that already existed. In condition position the literal is a *value*,
    // and a condition is a bool or a command — so it is refused by type rather than
    // silently branching, and the diagnostic names the fix.
    let condition = run_with_input("if 42 { puts t } else { puts f }\nputs after\n");
    let stderr = String::from_utf8_lossy(&condition.stderr);
    assert!(stderr.contains("an int is not a condition"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&condition.stdout), "after\n");

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

    // `true` / `false` are literals for the numeral's reason, even though a program
    // of each name exists: read as a value they are the boolean, so no `/usr/bin/true`
    // is forked to learn what everyone already knows.
    let word = run_with_input("func t() { true }\nfunc f() { false }\nputs t():repr f():repr\n");
    assert_eq!(String::from_utf8_lossy(&word.stdout), "true false\n");

    // The program is still reachable the way `./42` is — by a spelling that is not a
    // lone bare word.
    let program = run_with_input("func t() { command -- true\n $sh.status }\nputs t():repr\n");
    assert_eq!(String::from_utf8_lossy(&program.stdout), "0\n");

    // And a bare word that names a command and is *not* a literal still runs: its
    // output streams and a function body's result is its status, which is exactly
    // what `true` did before it became a literal.
    let ran = run_with_input("func p() { pwd }\nv = p()\nputs \"v=$v:repr\"\n");
    let ran = String::from_utf8_lossy(&ran.stdout);
    assert!(ran.ends_with("v=0\n"), "{ran:?}");
    assert!(ran.starts_with('/'), "pwd should have streamed: {ran:?}");

    // mesh has no float literals, so `3.5` is still just a word — and still a
    // command. Closing that would mean adding a type, not a parse rule.
    let float = run_with_input("func f() { 3.5 }\nv = f()\nputs after\n");
    assert!(
        String::from_utf8_lossy(&float.stderr).contains("command not found: 3.5"),
        "{:?}",
        float.stderr
    );

    // A *negative* literal lexes as the minus operator followed by `3` rather than as
    // one numeric word, so it used to miss this rule and `func f() { -3 }` reported
    // "command not found: -3" beside a `return -3` that carried the number. The rule
    // is asked of the parsed expression now, and the parser folds the sign into the
    // literal, so all three spellings agree.
    let carried = run_with_input(
        "func f() { -3 }\nfunc g() { return -3 }\nfunc h() { (-3) }\n\
         a = f()\nb = g()\nc = h()\nputs \"$a $b $c\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&carried.stdout), "-3 -3 -3\n");
    assert!(carried.stderr.is_empty(), "{:?}", carried.stderr);

    // Statement position keeps its redirect reading, and keeps it for both spellings
    // of the sign — `-3 > out` and `- 3 > out` are the same command line.
    for signed in ["-3 > out", "- 3 > out"] {
        let out = run_with_input(&format!("cd {}\n{signed}\nputs after\n", dir.display()));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("command not found:"),
            "{signed} still redirects in statement position: {:?}",
            out.stderr
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{signed}");
    }

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
            "for a in [1 2] {\n  match id(if true { break }) { _ => { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
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
            "for a in [1 2] {\n  match 7 { 7 if (if true { break }) => { puts LEAK }; _ => { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "value mode",
            "for a in [1 2] {\n  v = match 7 { 7 if (if true { break }) => { puts LEAK\n 1 }; _ => { puts LEAK\n 2 } }\n  puts LEAK-AFTER\n}\n",
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
            "for a in [1 2] {\n  match 1 { id(if true { break }) => { puts LEAK }; _ => { puts LEAK } }\n  puts LEAK-AFTER\n}\n",
        ),
        (
            "value mode",
            "for a in [1 2] {\n  v = match 1 { id(if true { break }) => 1; _ => { puts LEAK\n 2 } }\n  puts LEAK-AFTER\n}\n",
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
        "func ifc() { 7 + 0\n if (if true { false\n 1 == 1 }) { return } }\n         func whilec() { 7 + 0\n while (if true { false\n 1 == 1 }) { return } }\n         func matchc() { 7 + 0\n match (if true { false\n 9 + 0 }) { _ => { return } } }\n         func forc() { 7 + 0\n for i in (if true { false\n [1] }) { return } }\n         a = ifc()\nb = whilec()\nc = matchc()\nd = forc()\n         puts \"[$a][$b][$c][$d]\"\n",
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
fn a_builtin_value_call_cannot_be_a_function_name() {
    // `re(...)`, `style(...)` and the `glob` family answer with a built-in value, so
    // a `func` of one of those names would be reachable as a command but never as a
    // value call — reserve the names instead of shipping a function whose meaning
    // depends on how it is called. The error is recoverable: the next command still
    // runs.
    for name in ["re", "style", "link", "glob", "files", "dirs"] {
        let out = run_with_input(&format!("func {name}(x) {{ return $x }}\nputs after\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("`{name}` is a built-in value call")),
            "{stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    }

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
    // A bare `false` is the boolean literal, so that is what the `return` carries;
    // its status view is 1, which is what makes the `||` fire in the first place.
    let chained = run_with_input(
        "func f() { false || return }\nf && puts bad\nf || puts ok\nv = f()\nputs \"v=[$v]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&chained.stdout), "ok\nv=[false]\n");
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
        "func inner() { fail 7 }\nfunc outer() { 42 + 0\n inner\n return }\n\
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

    // A quoted scalar statement is a string *literal*, so the result it leaves for
    // a bare `return` is that string — status 0, since producing a value is
    // success. Quoting makes a value; `command -- "…"` is how a path with spaces
    // is run.
    let quoted =
        run_with_input("func f() { \"false\"\n return }\nf && puts ok\nv = f()\nputs \"v=[$v]\"\n");
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "ok\nv=[false]\n");

    // A compound that ran but produced no value results in the empty string — not
    // the result the statement before it recorded, and not its own status. That is
    // what the same construct yields in value position, so the two agree.
    let empty_compound = run_with_input(
        "func branch() { 5 + 0\n if true { }\n return }\n         func unbranched() { 5 + 0\n if false { 1 + 1 }\n return }\n         func elsewhere() { 5 + 0\n if false { 9 } else { }\n return }\n         func unmatched() { 5 + 0\n match 1 { 2 => { 3 + 3 } }\n return }\n         func unlooped() { 5 + 0\n while false { 1 + 1 }\n return }\n         a = branch()\nb = unbranched()\nc = elsewhere()\nd = unmatched()\ne = unlooped()\n         puts \"[$a][$b][$c][$d][$e]\"\n",
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
    // An expression statement's status is the view of its value (`DESIGN.md`):
    // only `false` fails, because `false` is mesh's "no result". Every other value
    // is a result, and producing one is success. That holds for a value call, for a
    // command-mode call whose body ends in an implicit value, and for a bare
    // expression — and `fail` is the separate spelling for naming a status.
    let out = run_with_input(
        "func f() { return false }\nfunc g() { 1 == 2 }\nfunc t() { 1 == 1 }\nfunc n() { return 3 }\nfunc bad() { fail 3 }\n\
         f() && puts bad-value-call\nf() || puts ok-value-call\n\
         g && puts bad-command-call\ng || puts ok-command-call\n\
         t && puts ok-true\n\
         n() && puts ok-integer\n\
         bad || puts ok-fail\n\
         1 == 2 || puts ok-bare\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok-value-call\nok-command-call\nok-true\nok-integer\nok-fail\nok-bare\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn an_integer_result_does_not_become_the_exit_status() {
    // A returned integer is a *result*, not a status: it reaches the value channel
    // and leaves the exit status at 0. `fail` is what names a status.
    let out = run_with_input("func n() { return 3 }\nn()\n");
    assert_eq!(out.status.code(), Some(0));
    // In *command* mode the status channel reaches the shell. A value call reads
    // the value channel instead, so it reports `false`'s status of 1.
    let failed = run_with_input("func n() { fail 3 }\nn\n");
    assert_eq!(failed.status.code(), Some(3));
    let called = run_with_input("func n() { fail 3 }\nn()\n");
    assert_eq!(called.status.code(), Some(1));
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

    // A command *is* backgroundable, and a path needing quotes reaches one through
    // `command --` now that a lone quoted word is a string literal rather than a
    // command — that stays a job, not an error.
    let command = run_with_input("command -- \"/bin/true\" &\nputs after\n");
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
            "f = func() { return 1 }\n/bin/echo $f\n",
            "$f: a function value has no text form",
        ),
        // An element of a spread.
        (
            "f = func() { return 1 }\nxs = [$f]\n/bin/echo ...$xs\n",
            "$xs: a function value has no text form",
        ),
        // `puts` renders real values, and still has nothing to render here.
        (
            "f = func() { return 1 }\nputs $f\n",
            "puts: a function value has no text form",
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
        "func f() { puts to-out\nnosuchcmd\nfail 7 }\n\
         r = f():capture\n\
         puts \"v=$r.value s=$r.status\"\n\
         puts \"out=[$r.out]\"\n\
         puts \"err=[$r.err]\"\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `fail` fills the status channel and leaves `false` — mesh's "no result" — in
    // the value channel.
    assert!(stdout.contains("v=false s=7"), "{stdout:?}");
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
    for name in ["cd", "exit", "func", "return", "jobs", "wait"] {
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
fn a_wrapper_func_forwards_an_undeclared_flag_instead_of_rejecting_it() {
    // The whole point of the marker: a wrapper does not know the callee's
    // grammar, so it cannot validate what it forwards. Validity is *relocated*
    // to the wrapped call, not dropped (`DESIGN.md` §"Functions").
    let out = run_with_input("wrapper func g(...xs) { puts $xs:repr }\ng --color=never -a x\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "['--color=never', '-a', 'x']\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_wrapper_func_forwards_help_rather_than_answering_it() {
    // `g --help` has to reach whatever `g` wraps; answering with mesh's
    // generated help would hide the callee's own.
    let out = run_with_input("wrapper func g(...xs) { puts $xs:repr }\ng --help\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "['--help']\n");
    // A plain `func` still gets the canned help, so the marker is what changed.
    let plain = run_with_input("func g(...xs) { puts $xs:repr }\ng --help\n");
    assert!(
        String::from_utf8_lossy(&plain.stdout).contains("Usage:"),
        "{:?}",
        String::from_utf8_lossy(&plain.stdout)
    );
}

#[test]
fn a_wrapper_func_forwards_the_terminator_too() {
    // `--` is data to a wrapper: the callee may need it (`grep -- -x`), and a
    // wrapper that ate it would change the command it forwards.
    let out = run_with_input("wrapper func g(...xs) { puts $xs:repr }\ng -- --x\ng a -- b\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "['--', '--x']\n['a', '--', 'b']\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_wrapper_func_still_binds_its_positionals() {
    // Disabling flag parsing is not disabling the signature: arity still holds,
    // and a leading positional still binds before the rest collects.
    let out = run_with_input(
        "wrapper func g(first, ...rest) { puts $first\nputs $rest:repr }\ng --a --b c\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "--a\n['--b', 'c']\n");
    let short = run_with_input("wrapper func g(a, b) { puts ok }\ng only\nputs after\n");
    assert!(
        String::from_utf8_lossy(&short.stderr).contains("expected 2 argument(s), got 1"),
        "{:?}",
        String::from_utf8_lossy(&short.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&short.stdout), "after\n");
}

#[test]
fn wrapper_is_contextual_not_reserved() {
    // Like `fork`, `wrapper` leads a definition only where `func` follows it, so
    // the word is still free as a variable, a function name, and a command.
    let out =
        run_with_input("wrapper = 1\nputs $wrapper\nfunc wrapper() { puts called }\nwrapper\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\ncalled\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn type_reports_a_wrapper_as_it_was_written() {
    // How a function treats a `--flag` is the part of its contract a caller most
    // needs to know, so `type` shows the marker rather than hiding it.
    let out = run_with_input("wrapper func g(...xs) { puts hi }\ntype g\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "g is a function\n    wrapper func g(...xs)\n"
    );
}

#[test]
fn a_wrapper_func_forwards_flags_in_a_value_call_too() {
    // The two call spellings are the same call, so the marker has to hold in both:
    // a value-mode `g(--color=never)` used to scan the token as an option and fail
    // on a flag the wrapper never declared.
    let out = run_with_input(
        "wrapper func g(...xs) { return $xs }\nputs g(--color=never, --, --help):repr\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "['--color=never', '--', '--help']\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_plain_func_still_scans_flags_in_a_value_call() {
    // The gate is the marker, not the call form: without `wrapper`, an undeclared
    // flag is still the caller's mistake.
    let out = run_with_input("func g(...xs) { return $xs }\nputs g(--color=never):repr\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown flag `--color`"),
        "{:?}",
        out.stderr
    );
}

#[test]
fn a_malformed_wrapper_header_quarantines_its_body() {
    // The reader judges a function header by scanning braces, and it has to see
    // the marked form as a header too: otherwise a typo dispatched the header on
    // the spot and the body's commands ran at top level.
    let out = run_with_input("wrapper func f(') {\nputs LEAKED\n}\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("LEAKED"), "{stdout}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unclosed"),
        "{:?}",
        out.stderr
    );
    // The plain spelling is the behavior being matched.
    let plain = run_with_input("func f(') {\nputs LEAKED\n}\n");
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_multiline_wrapper_body_still_reads_to_its_close() {
    // The counterpart: a well-formed wrapper written across lines must keep
    // buffering rather than dispatching at the first newline.
    let out = run_with_input("wrapper func g(...xs) {\nputs ok ...$xs\n}\ng --a b\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok --a b\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_wrapper_cannot_declare_a_flag_of_its_own() {
    // The marker says the function parses no flags, so declaring one is a
    // contradiction: help would list it and completion offer it while every
    // command-position `--force` went to `...rest`.
    let out = run_with_input("wrapper func g(--force, ...xs) { puts hi }\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot declare `--force`"), "{stderr}");
    // A plain `func` is unaffected.
    let ok = run_with_input("func p(--force, ...xs) { puts $force:repr }\np --force\n");
    assert_eq!(String::from_utf8_lossy(&ok.stdout), "true\n");
}

#[test]
fn an_alias_is_a_wrapper_func_that_forwards() {
    // The whole feature in one line: `alias co = …` is sugar for the wrapper you
    // would otherwise write out, so it takes arguments and forwards flags.
    // `--color=never` names no parameter of the generated `co(...args)`, so a
    // plain `func` would have rejected it here. (`puts` reads the `--` itself,
    // so the terminator is left out of this one — `a_wrapper_func_forwards_the
    // _terminator_too` covers it on the underlying form.)
    let out = run_with_input("alias g = puts grep\ng --color=never x\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "grep --color=never x\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn type_reports_an_alias_as_the_wrapper_it_desugars_to() {
    // Sugar, not a second mechanism: what is defined is a function, and `type`
    // says so rather than inventing an alias namespace to report from.
    let out = run_with_input("alias co = puts checkout\ntype co\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "co is a function\n    wrapper func co(...args)\n"
    );
}

#[test]
fn a_self_naming_alias_reaches_the_program() {
    // `alias grep = grep --color=auto` is the commonest alias there is, and a
    // literal desugaring would recurse forever, so a leading word equal to the
    // alias's own name is emitted as `command NAME`.
    let out = run_with_input("alias true = true\ntrue\nputs $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    // Quoting is not part of the question: a quoted command head still resolves
    // functions, so a bare-text-only check here recursed to the stack limit.
    for source in [
        "alias true = \"true\"\ntrue\nputs ok\n",
        "alias true = 'true'\ntrue\nputs ok\n",
    ] {
        let quoted = run_with_input(source);
        assert_eq!(String::from_utf8_lossy(&quoted.stdout), "ok\n");
        assert!(quoted.stderr.is_empty(), "{:?}", quoted.stderr);
    }
    // Only the *first* word: a later occurrence is an ordinary argument.
    let arg = run_with_input("alias e = puts e\ne x\n");
    assert_eq!(String::from_utf8_lossy(&arg.stdout), "e x\n");
}

#[test]
fn an_alias_cannot_take_a_reserved_name() {
    // `alias re = …` would define a command-position `re` while `re(x)` still
    // built a regex — the syntax-dependent meaning `func` refuses outright.
    for name in ["re", "style", "link", "glob", "files", "dirs"] {
        let out = run_with_input(&format!("alias {name} = puts wrapped\n"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot be a function name"),
            "{name}: {stderr}"
        );
    }
}

#[test]
fn alias_is_contextual_not_reserved() {
    // Like `wrapper` and `fork`, `alias` leads a definition only in the shape
    // that claims it, so the word stays free everywhere else.
    let out =
        run_with_input("alias = 1\nputs $alias\nfunc alias(x) { puts \"got $x\" }\nalias y\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\ngot y\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_bare_alias_points_at_the_spelling_that_works() {
    // The bash reflex lands here, and the note has to name the form that works
    // rather than the old "mesh has no aliases".
    let out = run_with_input("alias\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("alias ll = ls -l"), "{stderr}");
    let unalias = run_with_input("unalias ll\n");
    assert!(
        String::from_utf8_lossy(&unalias.stderr).contains("wrapper func"),
        "{:?}",
        unalias.stderr
    );
}

#[test]
fn a_quoted_alias_command_says_to_drop_the_quotes() {
    // bash needs `alias ll='ls -l'` because its body is a string; mesh's is
    // syntax, so the quotes make one word naming no program. Diagnosed rather
    // than left to `command not found: ls -l`, which reports the odd name
    // without saying the quotes caused it.
    for source in ["alias ll = 'ls -l'\n", "alias ll = \"ls -l\"\n"] {
        let out = run_with_input(source);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("alias NAME = ls -l"), "{stderr}");
    }
    // A quoted single word is fine — nothing is being run through a shell.
    let ok = run_with_input("alias e = \"puts\" hi\ne there\n");
    assert_eq!(String::from_utf8_lossy(&ok.stdout), "hi there\n");
    // So is a quoted argument.
    let arg = run_with_input("alias say = puts \"two words\"\nsay now\n");
    assert_eq!(String::from_utf8_lossy(&arg.stdout), "two words now\n");
}

#[test]
fn an_alias_needs_a_command_after_the_equals() {
    let out = run_with_input("alias co =\n");
    assert!(!out.stderr.is_empty(), "{:?}", out.stderr);
    // A guard belongs in a body, not on the definition.
    let guard = run_with_input("alias co = puts hi if true\n");
    let stderr = String::from_utf8_lossy(&guard.stderr);
    assert!(stderr.contains("wrapper func"), "{stderr}");
}

#[test]
fn help_explains_the_alias_shorthand() {
    let out = run_with_input("help alias\n");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("alias NAME = CMD ARG"),
        "{:?}",
        out.stdout
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn help_explains_the_wrapper_marker() {
    // A word the parser takes and `help` does not know is a reader being told,
    // falsely, that the word they just used is not syntax.
    let out = run_with_input("help wrapper\n");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("wrapper func NAME(…ARGS) { … }"),
        "{:?}",
        out.stdout
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
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
        "func f(x = if true { fail 7 }) { puts body }\nf && puts ok || puts caught\nputs after\n",
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
    let out = run_with_input("func failing() { fail 3 }\nfailing | cat\n");
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
fn real_resolves_symlinks_and_dot_segments_to_an_absolute_path() {
    // The `readlink -f` a config otherwise forks for, on a syscall the shell
    // already has. Rough edge 16 from the config port.
    let dir = fresh_dir("real_modifier");
    let target = dir.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, dir.join("link")).unwrap();
    // The temp root may itself sit under a symlink, so compare against the
    // resolved answer rather than the path we built.
    let resolved = std::fs::canonicalize(&target).unwrap();
    let resolved = resolved.display();

    // Quoted: a bare `..` is the range operator, so a dot-segment path is written
    // as the string it is.
    let out = run_with_input(&format!(
        "p = \"{}/./target/../link\"\nputs $p:real\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{resolved}\n"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A path modifier, so it maps element-wise over a list.
    let listed = run_with_input(&format!(
        "ps = [{0}/link {0}/target]\nputs ...$ps:real\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        format!("{resolved} {resolved}\n")
    );

    // Resolving is a syscall, so a path that is not there has no answer to give
    // and this errors rather than inventing one — as `:type` does, and unlike the
    // yes/no file tests, which a missing file can still answer with `false`.
    let missing = run_with_input(&format!("puts {}/nope:real\n", dir.display()));
    assert!(!missing.status.success());
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(err.contains(":real:"), "{err}");
    assert!(err.contains("nope"), "{err}");
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
fn quoted_path_with_spaces_runs_through_command() {
    // A quoted word is a string literal, so writing the path alone binds it rather
    // than running it. `command --` is the spelling that runs a path needing
    // quotes, and it is the escape hatch the bare/quoted rule leaves in place.
    let dir = fresh_dir("quoted command");
    let command = dir.join("say hello");
    std::fs::write(&command, "#!/bin/sh\nprintf 'ran\\n'\n").unwrap();
    let mut permissions = std::fs::metadata(&command).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&command, permissions).unwrap();
    let out = run_with_input(&format!("command -- \"{}\"\n", command.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Written alone it is the string, and nothing runs.
    let bound = run_with_input(&format!("p = \"{}\"\nputs $p:repr\n", command.display()));
    assert_eq!(
        String::from_utf8_lossy(&bound.stdout),
        format!("'{}'\n", command.display())
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
fn a_command_branch_streams_rather_than_becoming_the_if_expression_value() {
    // A block is not a `$(…)`. The branch's output goes where stdout goes — in
    // value position exactly as in statement position — and the `if` yields the
    // status of the command that produced no value, the same answer a `func` body
    // ending in a command gives. Bytes come from an explicit capture.
    let out = run_with_input(
        "french = true\ngreeting = if $french { printf bonjour } else { hi }\nputs \"<$greeting>\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bonjour<0>\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A **hard** lexical error inside a body — an invalid escape — does not decide
/// anything on its own. It sits inside a string, so it cannot change brace
/// structure, and the only question that matters is still whether the body's `}`
/// has arrived. Both answers have a way to be wrong:
///
/// - `}` present: the definition is **done**, so it dispatches. Waiting for a later
///   line to repair an invalid escape buffers forever behind a diagnostic that never
///   arrives — interactively the prompt could only be escaped by cancelling.
/// - `}` not yet: the definition is **still open**, so it stays quarantined.
///   Dispatching there ran the body's own later lines as top-level commands.
#[test]
fn a_hard_lexical_error_in_a_function_body_reports_without_leaking() {
    // Closed: the error is reported and the command after the definition runs.
    for input in [
        "func f() { puts \"\\z\" }\nputs LATER\n",
        "func f() {\n  puts \"\\z\"\n}\nputs LATER\n",
    ] {
        let out = run_with_input(input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "{input:?}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "LATER\n",
            "{input:?} swallowed the command after it: {stderr}"
        );
    }

    // Still open: the body's later lines belong to the quarantined definition and
    // must not run, whether the `}` eventually arrives or the input just ends.
    for input in [
        "func f() {\nputs \"\\z\"\nputs LEAKED\n}\nputs AFTER\n",
        "func f() { puts \"\\z\"\nputs LEAKED\n",
    ] {
        let out = run_with_input(input);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("syntax error") || !String::from_utf8_lossy(&out.stderr).is_empty(),
            "{input:?} reported nothing"
        );
        assert!(
            !stdout.contains("LEAKED"),
            "{input:?} leaked a body command to the top level: {stdout}"
        );
    }

    // And the definition that *does* close still runs its own commands afterwards.
    let out = run_with_input("func f() {\nputs \"\\z\"\nputs LEAKED\n}\nputs AFTER\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "AFTER\n");

    // The `}` is found however many errors precede it and whatever shape they take.
    // A **zero-width** diagnostic (`${}` reports an empty span) and an error count
    // past any fixed retry budget both defeated span-blanking recovery.
    let many = "\\z".repeat(200);
    for input in [
        "func f() { puts ${} }\nputs LATER\n".to_owned(),
        format!("func f() {{ puts \"{many}\" }}\nputs LATER\n"),
    ] {
        let out = run_with_input(&input);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "LATER\n",
            "a closed body was not seen past its errors: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The other half of that split: an **open** construct inside a body is one a later
/// line can still close, so it keeps buffering rather than dispatching mid-string.
#[test]
fn an_open_construct_in_a_function_body_keeps_buffering() {
    // A string that spans physical lines, and a heredoc body — both carry a `}`-free
    // stretch the reader must not mistake for the end of the definition.
    let out = run_with_input("func f() {\n  puts \"line one\nline two\"\n}\nf\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "line one\nline two\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_with_input("func f() {\n  cat << END\nhello\nEND\n}\nf\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And a `}` inside a string is not the body's close.
    let out = run_with_input("func f() {\n  puts \"a } b\"\n  puts second\n}\nf\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a } b\nsecond\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A tokenize failure *after* the body's `}` says nothing about the body — it has
/// already closed. Reading it as "still open" buffered a finished definition forever
/// and swallowed every command after it.
#[test]
fn a_close_before_a_tokenize_failure_does_not_swallow_later_commands() {
    let out = run_with_input("func f() {} puts \"\nputs LATER\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("syntax error"), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "LATER\n",
        "a finished definition swallowed the command after it: {stderr}"
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

/// The scalar file tests: `:exists` is `test -e`, `:type` is the `find -type`
/// word, `:read`/`:write` are `-r`/`-w`. Like `test` they dereference, so a live
/// symlink exists and a broken one does not — `:type` is the one that reports on
/// the link itself.
#[test]
fn file_tests_answer_questions_about_one_path() {
    use std::ffi::CString;

    let dir = fresh_dir("file_tests");
    std::fs::write(dir.join("plain.txt"), "x").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::os::unix::fs::symlink("plain.txt", dir.join("good.link")).unwrap();
    std::os::unix::fs::symlink("nowhere", dir.join("broken.link")).unwrap();
    let fifo = CString::new(dir.join("pipe").into_os_string().into_vec()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);

    let out = run_with_input(&format!(
        "cd {}\n\
         p = plain.txt\n\
         puts $p:exists $p:type $p:read $p:write\n\
         d = sub\n\
         puts $d:exists $d:type\n\
         good = good.link\n\
         puts $good:exists $good:type\n\
         bad = broken.link\n\
         puts $bad:exists $bad:type\n\
         f = pipe\n\
         puts $f:type\n\
         gone = nowhere\n\
         puts $gone:exists\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true file true true\ntrue dir\ntrue link\nfalse link\nfifo\nfalse\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The file-type filters keep a list's matching elements and drop the rest, and
/// chain for AND (`:f:x` is executable files). On a single path each is the bare
/// `test` predicate instead — which is what makes them usable as the callable a
/// `:filter` applies element by element, the equivalence `DESIGN.md` states.
#[test]
fn file_filters_keep_the_matching_list_elements_and_chain() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fresh_dir("file_filters");
    std::fs::write(dir.join("plain.txt"), "x").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::os::unix::fs::symlink("plain.txt", dir.join("good.link")).unwrap();

    let out = run_with_input(&format!(
        "cd {}\n\
         xs = [plain.txt sub run.sh good.link nowhere]\n\
         puts ...$xs:files\n\
         puts ...$xs:dirs\n\
         puts ...$xs:links\n\
         puts ...$xs:f:x\n\
         one = run.sh\n\
         puts $one:files $one:exec $one:dirs $one:links\n\
         same = $xs:filter(func(f) {{ $f:exec }})\n\
         puts ...$same\n\
         puts ...$xs:exec\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // `good.link` is a file because the filters dereference; `sub` is `:exec`
        // because a searchable directory carries the bit, which is why the
        // executable-files idiom needs `:f:x` rather than `:x` alone. The last two
        // lines are the modifier and its lambda spelling, and must agree.
        "plain.txt run.sh good.link\nsub\ngood.link\nrun.sh\ntrue true false false\nsub run.sh\nsub run.sh\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A file modifier asked of something that is not a path fails loud, and `:type`
/// has no word to report for a path that is not there.
#[test]
fn a_file_modifier_rejects_a_non_path_and_a_missing_type() {
    for (source, message) in [
        ("m = nowhere\nputs $m:type\n", "no such file: `nowhere`"),
        ("n = 3\nputs $n:exists\n", ":exists: requires a path"),
        (
            "xs = [a 1]\nputs ...$xs:files\n",
            ":files: requires a list of paths",
        ),
    ] {
        let out = run_with_input(source);
        assert!(!out.status.success(), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }
}

/// A bare `:name` is a **callable value** — the function that applies that
/// modifier — so a predicate or mapper can be handed the modifier directly instead
/// of a lambda that only forwards to it (`DESIGN.md`). This is the exact
/// equivalence `DESIGN.md` states, including its motivating example.
#[test]
fn a_bare_modifier_reference_is_a_callable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fresh_dir("modifier_ref");
    std::fs::write(dir.join("plain.txt"), "x").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = run_with_input(&format!(
        "cd {}\n\
         ps = [\"a/b.txt\" \"c/d.tar.gz\"]\n\
         stems = $ps:map(:stem)\n\
         bases = $ps:map(:base)\n\
         xs = [plain.txt run.sh nowhere]\n\
         runnable = $xs:filter(:exec)\n\
         present = $xs:filter(:files)\n\
         there = $xs:map(:exists)\n\
         through = :stem\n\
         one = $through(\"x/y.tar.gz\")\n\
         spread = $through(...[\"p/q.tar.gz\"])\n\
         inlist = [:stem]\n\
         listed = $inlist[0](\"m/n.tar.gz\")\n\
         still_empty = [:]\n\
         puts ...$stems\n\
         puts ...$bases\n\
         puts ...$runnable\n\
         puts ...$present\n\
         puts ...$there\n\
         puts $one $spread $listed $still_empty:len\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // The last line: a direct call, a spread call (a spread explodes into
        // arguments before the count is checked, as for a one-parameter lambda), a
        // reference traveling in a list, and `[:]` still being the empty map — the
        // two readings of a leading `[:` that have to coexist.
        "b d.tar\nb.txt d.tar.gz\nrun.sh\nplain.txt run.sh\ntrue true false\ny.tar q.tar n.tar 0\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A modifier reference is a function value like any other: no text form, and
/// identity — not the name — is what equality means, so `:stem` written twice is
/// two values, exactly as two identical lambdas are.
#[test]
fn a_modifier_reference_behaves_like_any_function_value() {
    let out = run_with_input(
        "x = :stem\n\
         y = :stem\n\
         if $x == $x { puts self-same }\n\
         if $x == $y { puts also-same } else { puts distinct }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "self-same\ndistinct\n"
    );

    let text = run_with_input("x = :stem\nputs $x\n");
    assert!(!text.status.success());
    assert!(
        String::from_utf8_lossy(&text.stderr).contains("a function value has no text form"),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );

    // Only an *expression* position takes a reference. A command word beginning
    // with `:` is still the literal text it has always been.
    let literal = run_with_input("puts :stem\n");
    assert_eq!(String::from_utf8_lossy(&literal.stdout), ":stem\n");
}

/// The names a reference cannot denote, and the ones it must not steal.
#[test]
fn a_modifier_reference_rejects_what_it_cannot_apply() {
    for (source, message) in [
        // `:join` needs a separator and `:map` a callable, so neither is a
        // one-argument function for a reference to denote.
        (
            "xs = [a b]\ny = $xs:map(:join)\n",
            "`:join` takes arguments, so it is not a one-argument function",
        ),
        (
            "xs = [a b]\ny = $xs:map(:filter)\n",
            "`:filter` takes arguments, so it is not a one-argument function",
        ),
        // A name `DESIGN.md` reserves but the engine cannot apply yet says so,
        // rather than being quietly dropped.
        (
            "xs = [a b]\ny = $xs:map(:sort)\n",
            "modifier :sort is not implemented yet",
        ),
        // The arity is the reference's own, not a signature's — but it is counted
        // *after* spreads expand, as a lambda's is.
        (
            "m = :stem\ny = $m()\n",
            "`:stem`: expected 1 argument, got 0",
        ),
        (
            "xs = [a/b c/d]\nm = :stem\ny = $m(...$xs)\n",
            "`:stem`: expected 1 argument, got 2",
        ),
        // A modifier has no options, so a flag or a named argument has nothing to
        // bind to.
        (
            "m = :stem\ny = $m(\"--x\")\n",
            "`:stem`: unknown flag `--x`",
        ),
        ("m = :stem\ny = $m(p: 1)\n", "`:stem`: unknown option `p:`"),
        // Only a **bare** `:name` is a reference. A quoted or escaped name composes
        // to the same text but must not keep the operator meaning the bare word has,
        // so there is no expression there at all.
        ("m = :'stem'\n", "syntax error"),
        ("m = :\\stem\n", "syntax error"),
        ("m = :\"stem\"\n", "syntax error"),
        // A transform reaching a predicate is still the loud error `:filter`
        // already gives — the footgun `DESIGN.md` raises, unchanged by the
        // shorter spelling that makes it easy to write.
        (
            "xs = [a/b.txt]\ny = $xs:filter(:stem)\n",
            "predicate must return a boolean",
        ),
        // Not a modifier name at all: there is no other reading of a leading `:`
        // in expression position, so it is a syntax error rather than literal text.
        ("xs = [a b]\ny = $xs:map(:nope)\n", "syntax error"),
    ] {
        let out = run_with_input(source);
        assert!(!out.status.success(), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }

    // A colon that belongs to a map key or a named argument is untouched: a
    // reference is written tight against its name, a key's colon is not.
    let untouched = run_with_input(
        "m = [stem: 1, dir: 2]\n\
         puts ...$m:keys\n\
         a = 1\n\
         b = 2\n\
         puts $a:$b\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&untouched.stdout),
        "stem dir\n1:2\n"
    );
    assert!(
        untouched.status.success(),
        "{}",
        String::from_utf8_lossy(&untouched.stderr)
    );
}

/// A reference means exactly what the postfix form means — and *which* modifier
/// `:name` is depends on the value it meets. On a regex the argument-free names are
/// the flags (`:i`, `:x`), while on a path `:x` is the executable filter. Both go
/// through one applier so a reference cannot answer differently from the `$r:i` it
/// is defined to mean.
#[test]
fn a_modifier_reference_follows_the_value_type_like_the_postfix_form() {
    let flags = run_with_input(
        "rs = [re('^ABC$')]\n\
         strict = abc ~ $rs[0]\n\
         loose = $rs:map(:i)\n\
         by_ref = abc ~ $loose[0]\n\
         lam = $rs:map(func(r) { $r:i })\n\
         by_lambda = abc ~ $lam[0]\n\
         puts $strict $by_ref $by_lambda\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&flags.stdout),
        // The unmodified pattern does not match; `:i` by reference and by lambda
        // both make it match, which is the equivalence.
        "false true true\n"
    );
    assert!(
        flags.status.success(),
        "{}",
        String::from_utf8_lossy(&flags.stderr)
    );

    // `:x` is the sharp case: extended syntax on a regex, the executable-file
    // filter on a path. Ignoring the pattern's space flips both answers.
    let extended = run_with_input(
        "rs = [re('^a b$')]\n\
         spaced = \"a b\" ~ $rs[0]\n\
         tight = ab ~ $rs[0]\n\
         ignored = $rs:map(:x)\n\
         still_spaced = \"a b\" ~ $ignored[0]\n\
         now_tight = ab ~ $ignored[0]\n\
         puts $spaced $tight $still_spaced $now_tight\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&extended.stdout),
        "true false false true\n"
    );

    // And a modifier that is not a regex flag reports what the postfix form
    // reports, rather than the not-implemented message a name-only lookup gives.
    let wrong = run_with_input("rs = [re('a')]\nbad = $rs:map(:stem)\n");
    assert!(!wrong.status.success());
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("modifier :stem is not valid for a regex"),
        "{}",
        String::from_utf8_lossy(&wrong.stderr)
    );
}

/// `:capture` wraps an **invocation** rather than transforming a value, so no
/// one-argument function corresponds to it and a reference cannot denote it.
///
/// Refusing it has to happen when the *value* is built, not when it is called: by
/// the time a call could reject it, the very invocation it was meant to capture has
/// already run — uncaptured, side effects and all.
#[test]
fn capture_is_not_a_modifier_a_reference_can_denote() {
    let out = run_with_input(
        "func f() { puts ran\n\
         return 7 }\n\
         m = :capture\n\
         r = $m(f())\n",
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(":capture applies to a call"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The assertion that matters: `f` never ran. Rejecting at the call instead would
    // leave `ran` here, having executed outside any capture.
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("ran"),
        "the would-be captured call ran anyway: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The postfix form is untouched — that is where `:capture` belongs, and there it
    // does capture, so `ran` lands in the record rather than on stdout.
    let postfix = run_with_input(
        "func f() { puts ran\n\
         return 7 }\n\
         r = f():capture\n\
         puts $r.value $r.out:len\n",
    );
    assert_eq!(String::from_utf8_lossy(&postfix.stdout), "7 4\n");
    assert!(
        postfix.status.success(),
        "{}",
        String::from_utf8_lossy(&postfix.stderr)
    );
}

/// A reference **call** starts a value, so it can open a condition or a statement —
/// `if :exists(…) { }` — not just sit on an assignment's right-hand side. Only the
/// attached `:name(…)` form is claimed, which nothing else can spell, so a command
/// word beginning with `:` keeps the reading it has always had.
#[test]
fn a_modifier_reference_call_can_open_a_condition() {
    let out = run_with_input(
        "if :exists(\"/tmp\") { puts there } else { puts missing }\n\
         if :exists(\"/no/such/path\") { puts there } else { puts missing }\n\
         if :exists(\"/tmp\") and :dirs(\"/tmp\") { puts both }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "there\nmissing\nboth\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Unattached, the colon is still command-word text — the reading `puts :stem`
    // and `$host:$port` depend on.
    let words = run_with_input("puts :stem\na = 1\nb = 2\nputs $a:$b\n");
    assert_eq!(String::from_utf8_lossy(&words.stdout), ":stem\n1:2\n");
    // `hi:there` is no longer among them: `:` + identifier is reserved by the
    // grammar, so an unknown name is refused rather than falling back to text.
    // `$a:$b` still reads as text because `$b` is not an identifier.
    let reserved = run_with_input("puts hi:there\n");
    assert!(
        String::from_utf8_lossy(&reserved.stderr).contains("`:there` is not a modifier"),
        "{}",
        String::from_utf8_lossy(&reserved.stderr)
    );
}

/// An out-of-loop `break` inside a reference call's argument is the caller's
/// error, and the statement has to recover from it exactly as the lambda call does
/// — the flag is cleared and the statement fails, rather than left set to stop the
/// enclosing function.
#[test]
fn an_invalid_break_in_a_reference_argument_recovers_like_a_lambda() {
    let reference = run_with_input(
        "func g() {\n\
         m = :stem\n\
         x = $m(if true { break })\n\
         puts after }\n\
         g\n\
         puts done\n",
    );
    let lambda = run_with_input(
        "func g() {\n\
         f = func(p) { $p }\n\
         x = $f(if true { break })\n\
         puts after }\n\
         g\n\
         puts done\n",
    );
    // The point: both keep running. Leaving `shell.control` set stops `g` at the
    // failed statement, so `after` never prints.
    assert_eq!(String::from_utf8_lossy(&reference.stdout), "after\ndone\n");
    assert_eq!(
        String::from_utf8_lossy(&reference.stdout),
        String::from_utf8_lossy(&lambda.stdout)
    );
    assert!(
        String::from_utf8_lossy(&reference.stderr).contains("break: not inside a loop"),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    // With a loop to leave, the `break` is honored rather than reported.
    let in_loop = run_with_input(
        "for i in [1 2] {\n\
         m = :stem\n\
         x = $m(if true { break })\n\
         puts unreached }\n\
         puts loop-done\n",
    );
    assert_eq!(String::from_utf8_lossy(&in_loop.stdout), "loop-done\n");
    assert!(in_loop.stderr.is_empty(), "{:?}", in_loop.stderr);
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
fn a_failing_capture_binds_its_output_and_lends_the_assignment_its_status() {
    // The bash idiom, and the reason the bytes are kept: a nonzero exit is a
    // *result* here, and the output is what was asked for.
    // The status is read into a name first: `$sh.status` is the *last* statement's,
    // so a `puts` in between would report its own success — as it would in bash.
    let out = run_with_input(
        "x = $(sh -c 'echo kept; exit 3')\nst = $sh.status\nputs \"x=[$x] status=$st\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x=[kept] status=3\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn if_binding_a_capture_branches_on_the_status_with_the_output_bound() {
    // `if out = $(cmd)` reads as it does in bash: the status picks the branch and
    // the output is bound on both, which is what makes the failing branch useful.
    let differs = run_with_input(
        "if out = $(sh -c 'echo changed; exit 1') { puts \"same\" } else { puts \"differ=[$out]\" }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&differs.stdout),
        "differ=[changed]\n"
    );

    let same =
        run_with_input("if out = $(echo nothing) { puts \"same=[$out]\" } else { puts differ }\n");
    assert_eq!(String::from_utf8_lossy(&same.stdout), "same=[nothing]\n");
}

#[test]
fn text_after_a_capture_does_not_displace_its_status() {
    // Trailing text and variables run nothing, so they cannot take the status from
    // the capture before them — `x = "$(sh -c 'exit 4')suffix"` is 4 in bash and
    // here. Raised in review: requiring the capture to be the *final* piece
    // reported 0. Only a piece that executes can displace it.
    let suffix =
        run_with_input("x = \"$(sh -c 'exit 4')suffix\"\nst = $sh.status\nputs \"$st/[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&suffix.stdout), "4/[suffix]\n");

    let variable =
        run_with_input("v = V\nx = \"$(sh -c 'exit 5')$v\"\nst = $sh.status\nputs \"$st/[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&variable.stdout), "5/[V]\n");

    // A capture with only text before it is still the last executing piece.
    let prefix = run_with_input("x = \"pre$(sh -c 'exit 6')\"\nst = $sh.status\nputs $st\n");
    assert_eq!(String::from_utf8_lossy(&prefix.stdout), "6\n");

    // Nothing executes at all, so there is no status to take.
    let plain = run_with_input("x = \"plain\"\nst = $sh.status\nputs $st\n");
    assert_eq!(String::from_utf8_lossy(&plain.stdout), "0\n");
}

#[test]
fn the_last_capture_in_a_right_hand_side_decides_the_status() {
    // bash's rule: `x=$(false)$(true)` is 0. Anything else would make the status
    // depend on which capture a reader happened to notice first.
    let out =
        run_with_input("x = \"$(sh -c 'exit 4')$(echo ok)\"\nputs \"status=$sh.status x=[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "status=0 x=[ok]\n");

    let reversed =
        run_with_input("x = \"$(echo ok)$(sh -c 'exit 4')\"\nputs \"status=$sh.status x=[$x]\"\n");
    assert_eq!(
        String::from_utf8_lossy(&reversed.stdout),
        "status=4 x=[ok]\n"
    );
}

#[test]
fn a_capture_inside_a_callee_does_not_decide_the_callers_status() {
    // The callee's internals are not the caller's right-hand side: `f` running a
    // failing capture and then returning a value has succeeded, so `if x = f()`
    // must take the then-branch. Raised in review on the capture-status change.
    let out = run_with_input(
        "func f() { puts $(sh -c 'exit 3')\n  return ok }\nif x = f() { puts \"then=[$x]\" } else { puts \"else=[$x]\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\nthen=[ok]\n");

    // The same through a lambda, which reaches the callee half by another route.
    let lambda = run_with_input(
        "g = func() { puts $(sh -c 'exit 3')\n  return ok }\nif y = $g() { puts \"then=[$y]\" } else { puts \"else=[$y]\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&lambda.stdout), "\nthen=[ok]\n");
}

#[test]
fn every_assignment_form_takes_its_captures_status() {
    // `$env.K = $(cmd)` and `$m.k = $(cmd)` bind the output like a plain
    // assignment, so they have to report the command's status too. Raised in
    // review — only the plain arm consumed it at first.
    let out = run_with_input(
        "$env.MESH_T = $(sh -c 'echo v; exit 3')\nst = $sh.status\nputs \"env=$st/[$env.MESH_T]\"\n\
         m = [k: old]\n$m.k = $(sh -c 'echo w; exit 4')\nmt = $sh.status\nputs \"member=$mt/[$m.k]\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "env=3/[v]\nmember=4/[w]\n"
    );
}

#[test]
fn a_command_supersedes_a_capture_interpolated_into_its_arguments() {
    // The documented rule: a command reports its own status, so an interpolated
    // capture's failure is not recoverable afterward. That has to hold inside an
    // assignment's right-hand side too — the successful `puts` overwrites the
    // capture's 3, so the binding succeeds. Raised in review.
    let out = run_with_input(
        "if x = if true { puts \"[$(sh -c 'exit 3')]\"\n  \"ok\" } { puts \"then=[$x]\" } else { puts \"else=[$x]\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[]\nthen=[ok]\n");

    // The bare capture is unaffected: `capture_source` records after its own body
    // has run, so the clear inside cannot erase what the capture itself reports.
    let bare =
        run_with_input("x = $(sh -c 'echo v; exit 3')\nst = $sh.status\nputs \"$st/[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "3/[v]\n");
}

#[test]
fn a_list_pattern_condition_does_not_leak_its_captures_status() {
    // A list-pattern condition asks whether the value has the requested shape —
    // the status is no part of that — so a capture inside it is discarded rather
    // than consumed. It still has to be cleared, or it escapes and decides the
    // enclosing assignment. Raised in review.
    let out = run_with_input(
        "if result = if [v] = [$(sh -c 'echo ok; exit 3')] { \"matched\" } else { \"miss\" } { puts \"then=[$result]/[$v]\" } else { puts \"else=[$result]/[$v]\" }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "then=[matched]/[ok]\n"
    );

    // The documented shape rule is untouched: a mismatch still selects `else`.
    let mismatch = run_with_input("if [a b] = [1] { puts matched } else { puts mismatch }\n");
    assert_eq!(String::from_utf8_lossy(&mismatch.stdout), "mismatch\n");
}

#[test]
fn a_capture_as_an_argument_to_a_captured_call_does_not_decide_the_assignment() {
    // `puts($(…)):capture` is a value expression, not a capture, so the argument's
    // status is not the assignment's — the record's own `.status` is. Raised in
    // review, and the case that no amount of clearing reached, since command-form
    // `:capture` runs the command by a path of its own.
    let out = run_with_input(
        "if r = puts($(sh -c 'exit 3')):capture { puts \"THEN/$r.status\" } else { puts \"ELSE/$r.status\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "THEN/0\n");
}

#[test]
fn an_evaluation_error_in_a_capture_is_not_a_status_to_carry() {
    // Yielding the bytes for a nonzero status must not extend to an *invalid
    // program*: those are the two channels `AGENTS.md` asks to keep apart, and a
    // capture is where they meet. `Step::Error` is what tells them apart — without
    // it, `x = $(puts $nope)` bound the empty string and carried on. Raised in
    // review as the one P1.
    let assigned = run_with_input("x = $(puts $nope)\nputs \"bound=[$x]\"\nputs done\n");
    let err = String::from_utf8_lossy(&assigned.stderr);
    assert!(err.contains("nope: unbound variable"), "{err}");
    // `x` never bound, so reading it is the second error — and the statement after
    // still runs, which is how mesh recovers from any evaluation error.
    assert!(err.contains("x: unbound variable"), "{err}");
    assert_eq!(String::from_utf8_lossy(&assigned.stdout), "done\n");

    // Interpolated, the statement is abandoned exactly as the same error outside a
    // capture abandons it — nothing half-printed.
    let interpolated = run_with_input("puts \"a[$(puts $nope)]b\"\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&interpolated.stdout), "after\n");

    // The reference: the same error with no capture involved behaves identically.
    let plain = run_with_input("puts \"a[$nope]b\"\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&plain.stdout), "after\n");
}

#[test]
fn an_evaluation_error_does_not_stop_the_body_it_is_in() {
    // An error abandons its statement, not the block — pinned because `Step::Error`
    // travels the same paths control flow does, and stopping the body would be an
    // easy way to get it wrong.
    let out = run_with_input("func f() { $nope:len\n  puts reached }\nf\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "reached\nafter\n");
}

#[test]
fn a_status_transparent_modifier_keeps_the_captures_status() {
    // `:upper` runs nothing, so the capture is still the last thing that recorded a
    // status — `$(cmd):upper` answers for `cmd`. Raised in review.
    let upper = run_with_input(
        "if x = $(sh -c 'echo kept; exit 3'):upper { puts \"THEN=[$x]\" } else { puts \"ELSE=[$x]\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&upper.stdout), "ELSE=[KEPT]\n");

    // An argument that runs nothing keeps it transparent too.
    let split = run_with_input(
        "x = $(sh -c 'echo a:b; exit 3'):split(\":\")\nst = $sh.status\nputs \"$st/$x:len\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&split.stdout), "3/2\n");

    // An index runs nothing either, so a capture reached through one keeps its
    // status. Raised in review.
    let indexed = run_with_input(
        "if x = $(sh -c 'echo a:b; exit 3'):split(\":\")[0] { puts \"THEN=[$x]\" } else { puts \"ELSE=[$x]\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&indexed.stdout), "ELSE=[a]\n");

    // Reaching into a value runs nothing, so `($m).sep` is as inert as `$sep` —
    // how the separator is *accessed* must not change the capture's status.
    let member = run_with_input(
        "m = [sep: \":\"]\nx = $(sh -c 'echo a:b; exit 3'):split(($m).sep)\nst = $sh.status\nputs \"$st/$x:len\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&member.stdout), "3/2\n");

    // A modifier that *invokes* is not transparent: the callable ran after the
    // capture, so it is what recorded last.
    let mapped = run_with_input(
        "xs = $(sh -c 'echo a; exit 3'):split(\"\\n\"):map(func(s) { $s })\nputs $sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&mapped.stdout), "0\n");
}

#[test]
fn an_evaluation_error_from_any_path_aborts_a_capture() {
    // Not just the unbound-variable path: arithmetic, indexing and `$env` misses
    // all report and fail, and each has to be an error rather than a status or the
    // capture yields an empty string over an invalid program. Raised in review.
    for body in [
        "puts (1 + true)",
        "puts $env.MESH_DEFINITELY_NOT_SET_XYZ",
        "xs = [1]\n  puts $xs[9]",
        // Invocation failures too — arity and unknown-option errors are invalid
        // calls, not commands that answered. Raised in review as a separate path.
        "func f(a) { $a }\n  f()",
        "func g(a) { $a }\n  g(1, --nope)",
        // Rejections raised before anything runs: a bad `command` option, a parse
        // failure, and a backgrounded conditional list. Raised in review.
        "command --hepl ls",
        "true && false &",
    ] {
        let out = run_with_input(&format!("x = $({body})\nputs \"bound=[$x]\"\nputs done\n"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout, "done\n", "body was: {body}");
    }
}

#[test]
fn a_capture_that_ran_nothing_reports_its_own_success() {
    // `$()` ran nothing, so it has no status of its own — the assignment reports 0
    // rather than whatever happened to run before it. Raised in review.
    let empty = run_with_input("false\nx = $()\nst = $sh.status\nputs \"st=$st x=[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&empty.stdout), "st=0 x=[]\n");

    // Same for a body that is not empty but whose every statement a guard skipped —
    // "ran nothing" is a question about execution, not about the source text.
    let skipped = run_with_input(
        "false\nx = $(puts skipped if false)\nst = $sh.status\nputs \"st=$st x=[$x]\"\n",
    );
    assert_eq!(String::from_utf8_lossy(&skipped.stdout), "st=0 x=[]\n");
}

#[test]
fn a_skipped_statement_does_not_clear_an_earlier_error() {
    // A guard-skipped trailing statement executes nothing, so it cannot turn an
    // invalid program back into an answer. Raised in review.
    let out = run_with_input(
        "x = $(puts $nope\n  puts skipped if false)\nputs \"bound=[$x]\"\nputs done\n",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nope: unbound variable"), "{err}");
    assert!(err.contains("x: unbound variable"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}

#[test]
fn an_evaluation_error_does_not_stop_the_loop_it_is_in() {
    // A loop sequences statements exactly as a source does, so an error ends the
    // *pass* and the next one still runs. Raised in review: the loops read any
    // non-`Continue` step as unwinding, which made `Step::Error` stop them after
    // one pass.
    let looped = run_with_input("for x in [1 2 3] { puts $x\n  puts $missing }\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&looped.stdout),
        "1\n2\n3\nafter\n",
        "{}",
        String::from_utf8_lossy(&looped.stderr)
    );

    let whiled = run_with_input(
        "n = 0\nwhile $n < 3 { n = $n + 1\n  puts $n\n  puts $missing }\nputs after\n",
    );
    assert_eq!(String::from_utf8_lossy(&whiled.stdout), "1\n2\n3\nafter\n");

    // The classification still travels out, so a capture around a loop whose last
    // pass ended invalid rejects rather than binding the partial output.
    let captured = run_with_input("x = $(for i in [1 2] { puts $missing })\nputs done\n");
    let err = String::from_utf8_lossy(&captured.stderr);
    assert!(err.contains("missing: unbound variable"), "{err}");
    assert_eq!(String::from_utf8_lossy(&captured.stdout), "done\n");
}

#[test]
fn an_evaluation_error_in_a_startup_file_does_not_skip_the_rest() {
    // `env.mesh` failing is not a reason to abandon `login.mesh` and `rc.mesh`; the
    // error is reported and the sequence goes on, as it does for a failing command
    // in the same place. Raised in review.
    let home = fresh_dir("startup_error");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("env.mesh"), "puts env\nputs $broken\n").unwrap();
    std::fs::write(config.join("login.mesh"), "puts login\n").unwrap();
    let main = home.join("main.mesh");
    std::fs::write(&main, "puts script\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .arg("--login")
        .arg(main.to_str().unwrap())
        .env("XDG_CONFIG_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("run a login shell");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("broken: unbound variable"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "env\nlogin\nscript\n");
}

#[test]
fn a_callee_that_ends_on_an_evaluation_error_is_not_a_value() {
    // Called for its value, a body that ends invalid has no answer to give: it
    // aborts the caller's statement rather than handing back the empty string.
    // Raised in review as a P1 — `x = $(f())` bound `""` and carried on.
    let captured =
        run_with_input("func f() { puts $nope }\nx = $(f())\nputs \"bound=[$x]\"\nputs done\n");
    let err = String::from_utf8_lossy(&captured.stderr);
    assert!(err.contains("nope: unbound variable"), "{err}");
    // Never bound, so reading it is the second error — and `done` still runs.
    assert!(err.contains("x: unbound variable"), "{err}");
    assert_eq!(String::from_utf8_lossy(&captured.stdout), "done\n");

    // A later statement that *does* execute answers for the body, so the same
    // callee ending cleanly is an ordinary value again.
    let recovered =
        run_with_input("func f() { puts $nope\n  puts kept }\nx = $(f())\nputs \"[$x]\"\n");
    assert_eq!(String::from_utf8_lossy(&recovered.stdout), "[kept]\n");
}

#[test]
fn a_capture_status_does_not_leak_to_a_later_assignment() {
    // The recorded status belongs to the right-hand side being evaluated, so an
    // ordinary assignment after a failing capture reports its own success.
    let out = run_with_input("x = $(sh -c 'exit 3')\ny = 5\nputs \"status=$sh.status\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "status=0\n");
}

#[test]
fn a_capture_that_returns_still_unwinds_rather_than_yielding() {
    // `return` inside a capture is the body unwinding, not a status, so it leaves
    // the function rather than binding a value.
    let out = run_with_input(
        "func f() { x = $(return 1; echo unreachable)\n  puts \"bound=[$x]\" }\nf\nputs after\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
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
fn words_splits_a_column_padded_line_into_its_columns() {
    // The case `:words` exists for: `getent`, `ip -o` and `df` all pad their
    // columns, so `:split(" ")` on one of their lines yields empty fields between
    // the real ones and every index after the first is wrong.
    let out = run_with_input(
        "line = \"root   x  0  0\"\nputs $line:split(\" \"):len\nputs $line:words:len\nputs $line:words:get(2, \"-\")\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "8\n4\n0\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn words_ignores_leading_and_trailing_whitespace() {
    // Unlike `:split`, which drops only the *trailing* empty run, `:words` yields
    // no empty element at either end.
    let out = run_with_input("a = \"  x y  \"\nputs $a:words:len\nputs $a:words:first\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\nx\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn words_binds_through_a_list_pattern() {
    // The shape that replaces bash's `read a b c`, which is what a config reaches
    // for when it takes a line apart.
    let out = run_with_input("[user _ uid] = \"root  x   0\":words\nputs \"$user/$uid\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "root/0\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn words_is_a_split_modifier_so_it_refuses_a_list() {
    // Same rule as `:split`, deliberately: the two are one family, and mapping
    // element-wise for one and not the other would be a trap. `:map(:words)` is
    // how a list of lines is taken apart.
    let out = run_with_input("xs = [\"a b\" \"c d\"]\nys = $xs:words\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("requires a string"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
}

#[test]
fn words_works_as_a_callable_reference() {
    // Argument-free, so it is usable as a bare `:mod` reference — which is what
    // makes the list-of-lines case a one-liner rather than a loop.
    let out = run_with_input("xs = [\"a  b\" \"c   d\"]\nputs $xs:map(:words):len\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");
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

/// `:` + identifier is reserved by the **grammar**, not gated on a name list, so a
/// name the vocabulary does not hold is refused rather than falling back to literal
/// text. The old fallback made the reserved set grow silently with every modifier
/// added — `img:raw` was text until `:raw` existed, then quietly was not — and the
/// failure landed on whoever upgraded rather than on whoever wrote the line.
#[test]
fn an_unknown_modifier_name_is_refused_rather_than_literal() {
    let out = run_with_input("host = example\nputs $host:port\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`:port` is not a modifier"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    // The message names both escapes, because quoting the *subject* is the reflex and
    // it does not work: the colon has to be inside the quotes, or the name braced.
    let out = run_with_input(
        "host = example\n\
         puts \"host:port\"\n\
         puts \"${host}:port\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "host:port\nexample:port\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A name the vocabulary *does* hold but the engine cannot apply yet is a
    // different thing, and still reports at run time rather than at parse time.
    let out = run_with_input("xs = [a b]\ny = $xs:map(:sort)\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not implemented yet"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
           [verb ...args] if $verb == start => [$verb ...$args]\n\
           _ => [wrong]\n\
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
           *.txt => text\n\
           *.md | *.markdown => markdown\n\
           _ => other\n\
         }\n\
         number = match 7 { 1..=9 => digit; _ => other }\n\
         exact = match 42 { 42 => integer; _ => wrong }\n\
         regex = match README.md { /^README/ => readme; _ => wrong }\n\
         first = match file.txt { * => broad; *.txt => narrow }\n\
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
           [verb value] if $verb == take => { puts wrong }\n\
           [verb value] if $value == payload => { puts $verb $value }\n\
           _ => { puts wrong }\n\
         }\n\
         empty = match absent { present => wrong }\n\
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
fn an_arm_value_is_a_value_but_an_arm_block_runs_commands() {
    // The arrow decides the context: `=> word` is a value, so a bare word is a
    // string even when a command of that name exists, while `=> { word }` is a
    // block, so the same word runs.
    let out = run_with_input(
        "label = match 1 { 1 => echo }\n\
         puts \"<$label>\"\n\
         match 1 { 1 => { echo ran } }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "<echo>\nran\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_attached_redirection_is_not_read_as_a_match_arrow() {
    // `=>` needs a boundary on each side, like the other value operators, so an
    // attached redirection keeps parsing as a word and a redirect: `value=>out`
    // is `value=` written to `out`, not a fat arrow.
    let dir = fresh_dir("attached_redirect_arrow");
    let out = run_with_input(&format!("cd {}\nputs value=>out\ncat out\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "value=\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_block_arms_value_still_follows_the_block_value_rules() {
    // A `=> { … }` block yields a value the way any block does. Inside braces a
    // bare word is a command whatever its arity, so `{ echo }` *runs* `echo` — its
    // output streams and the block's value is the status, not the bytes — while a
    // quoted word is the string literal.
    let out = run_with_input(
        "word = match 1 { 1 => { echo } }\n\
         run = match 1 { 1 => { echo two words } }\n\
         quoted = match 1 { 1 => { \"echo\" } }\n\
         puts \"<$word>\" \"<$run>\" \"<$quoted>\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "\ntwo words\n<0> <0> <echo>\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn arms_are_terminator_separated_so_semicolons_put_them_on_one_line() {
    let out = run_with_input(
        "one = match 2 { 1 => a; 2 => b; _ => c }\n\
         two = match 9 { 1 => a; 2 => b; _ => c }\n\
         guarded = match 7 { n if 1 == 2 => wrong; 7 => right; _ => other }\n\
         puts $one $two $guarded\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b c right\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_arm_without_an_arrow_or_a_separator_is_a_syntax_error() {
    // The arrow is mandatory — it is what ends the pattern and its guard — and a
    // separator between arms is required, so arms cannot run together.
    for source in ["match 1 { 1 { puts x } }\n", "match 1 { 1 => a 2 => b }\n"] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "expected a syntax error for {source:?}, got {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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

    // `exec` refuses a file any process still holds open for writing, with
    // `ETXTBSY`. Nothing here holds it — the write is closed above — but a fork
    // elsewhere in the suite inherits whatever was open at the instant it forked,
    // and mesh forks in-shell stages that never `exec`, so such a copy can
    // outlive this file's write by the whole life of a background job. The
    // condition clears on its own, so wait it out rather than call it a failure.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let out = loop {
        let attempt = Command::new(&path)
            .arg("world")
            .env("XDG_CONFIG_HOME", isolated_config_home())
            .stdin(Stdio::null())
            .output();
        match attempt {
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => break other.expect("run script through its shebang"),
        }
    };
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

/// `$m.key = v` and `$xs[i] = v` write *into* a bound collection, generalizing the
/// `$env.KEY` form that was until now the only member assignment there was. Nested
/// paths mix members and indices, `+=` combines rather than replaces, and a
/// subscript may itself be a variable.
#[test]
fn a_member_assignment_writes_into_a_map_or_list() {
    let out = run_with_input(
        "m = [a: 1, b: 2]\n\
         $m.a = 9\n\
         $m.c = 3\n\
         $m.a += 5\n\
         puts $m.a $m.b $m.c\n\
         puts ...$m:keys\n\
         xs = [10 20 30]\n\
         $xs[0] = 99\n\
         $xs[-1] = 77\n\
         $xs[1] += 5\n\
         puts ...$xs\n\
         k = \"2\"\n\
         $xs[$k] = 5\n\
         puts ...$xs\n\
         nested = [outer: [inner: 1], rows: [1 2]]\n\
         $nested.outer.inner = 42\n\
         $nested.rows[1] = 7\n\
         puts $nested.outer.inner\n\
         puts ...$nested.rows\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // A new key appends in insertion order; a negative index counts from the
        // end, as it does on the way in.
        "14 2 3\na b c\n99 25 77\n99 25 5\n42\n1 7\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A member write follows the same **local-by-default** rule every other
/// assignment does: inside a function it shadows the outer binding rather than
/// reaching through to it, exactly as `n += …` and `n = …` already do. `global`
/// remains the way to write the outer one.
#[test]
fn a_member_assignment_is_local_by_default_like_every_other() {
    let out = run_with_input(
        "m = [a: 1]\n\
         n = [a: 1]\n\
         func member() { $m.a = 99\n\
         puts member-inside $m.a }\n\
         func append() { n += [b: 2]\n\
         puts append-inside $n:len }\n\
         func rebind() { global m = [a: 7]\n\
         puts rebind-inside $m.a }\n\
         member\n\
         puts member-outside $m.a\n\
         append\n\
         puts append-outside $n:len\n\
         rebind\n\
         puts rebind-outside $m.a\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // The member write and the `+=` agree: both shadow. Only `global` carries
        // out of the function.
        "member-inside 99\nmember-outside 1\n\
         append-inside 2\nappend-outside 1\n\
         rebind-inside 7\nrebind-outside 7\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Nothing along a member path is conjured: a missing intermediate key is an
/// error, not an empty map created to hold the write. The **last** step is the one
/// exception, and only for a map, where adding a key is what assignment is for.
#[test]
fn a_member_assignment_fails_loud_rather_than_creating_structure() {
    for (source, message) in [
        // An intermediate key that is not there.
        (
            "m = [a: 1]\n$m.typo.deep = 1\n",
            "$m.typo.deep: no `typo` in this map",
        ),
        // `+=` has nothing to combine with when the key is absent, so it is not
        // quietly a first write.
        ("m = [a: 1]\n$m.new += 1\n", "$m.new: no `new` in this map"),
        // A list is written in place; there is no value to fill a gap with.
        (
            "xs = [1 2]\n$xs[5] = 1\n",
            "$xs[5]: list index out of range",
        ),
        (
            "xs = [1 2]\n$xs[-9] = 1\n",
            "$xs[-9]: list index out of range",
        ),
        (
            "s = \"text\"\n$s.key = 1\n",
            "$s.key: cannot assign into a string",
        ),
        (
            "xs = [1 2]\n$xs.key = 1\n",
            "$xs.key: a list has no `key` member",
        ),
        // A slice names a copy of a run of elements, not a place.
        (
            "xs = [1 2 3]\n$xs[0..2] = 9\n",
            "$xs[0..2]: cannot assign to a slice",
        ),
        ("$nope.key = 1\n", "nope: unbound variable"),
        // `+=` type rules are the whole-variable ones, because both go through
        // one `append_into`.
        (
            "xs = [1 2]\n$xs[0] += \"s\"\n",
            "$xs[0]: can only add an integer to an integer",
        ),
    ] {
        let out = run_with_input(source);
        assert!(!out.status.success(), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }

    // A right-hand side that raised `break` produced no value, so the place keeps
    // what it had rather than taking a placeholder.
    let broke =
        run_with_input("m = [a: 1]\nfor i in [1] { $m.a = (if true { break }) }\nputs $m.a\n");
    assert_eq!(String::from_utf8_lossy(&broke.stdout), "1\n");
}

/// The reserved namespaces keep the handling they had: `$env.KEY` is still the
/// byte-boundary environment write, and neither `$env` nor `$sh` becomes an
/// ordinary place just because member assignment now exists.
#[test]
fn member_assignment_leaves_the_reserved_namespaces_alone() {
    let env = run_with_input("$env.MESH_TEST_M = hello\nputs $env.MESH_TEST_M\n");
    assert_eq!(String::from_utf8_lossy(&env.stdout), "hello\n");

    // Unchanged from before: an index or modifier on `$env` is not an assignment
    // target, and the message for one comes from parsing it as an expression.
    for source in ["$env.PATH[0] = x\n", "$env.PATH:dedup = x\n"] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(2), "{source}");
    }

    // `$sh` is a place — `$sh.options` is writable — so a write to a runtime
    // entry is refused at **run** time, by name, rather than being a syntax error
    // about the `=` that named neither the entry nor the reason.
    for source in ["$sh.name = x\n", "$sh.args[0] = x\n"] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(1), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("is read-only"), "{source} gave {stderr}");
    }
}

/// `global $m.key = value` writes **into** the session-global binding rather than
/// shadowing it — the escape hatch a local-by-default member write needs, so a
/// function can modify a caller's collection instead of copying it.
#[test]
fn a_global_member_assignment_writes_through_to_the_outer_binding() {
    let out = run_with_input(
        "m = [a: 1]\n\
         xs = [1 2]\n\
         val = 9\n\
         func write() { global $m.key = $val\n\
         global $m.a += 5\n\
         global $xs[0] = 9 }\n\
         write\n\
         puts $m.a $m.key\n\
         puts ...$xs\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "6 9\n9 2\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `global` names the global scope, so it writes there even where a local
    // shadows the name — the local keeps what it had.
    let shadowed = run_with_input(
        "m = [a: 1]\n\
         func write() { m = [a: 100]\n\
         global $m.a = 7\n\
         puts local $m.a }\n\
         write\n\
         puts global $m.a\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&shadowed.stdout),
        "local 100\nglobal 7\n"
    );

    // The global scope *is* the target, so there is nothing to copy inward: an
    // unbound name is simply an error.
    let unbound = run_with_input("func write() { global $nope.k = 1 }\nwrite\nputs after\n");
    assert!(
        String::from_utf8_lossy(&unbound.stderr).contains("nope: unbound variable"),
        "{}",
        String::from_utf8_lossy(&unbound.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unbound.stdout), "after\n");
}

/// A **failed** write must leave scope state exactly as it found it. Where the
/// name is only bound further out, seeding the local shadow before walking the path
/// meant an errored statement still shadowed the outer binding — so a later
/// `global $m.… = …` wrote the global while reads went on seeing the stale copy.
///
/// The whole-variable `+=` had the same hole, since both go through one
/// `Vars::update` now; the test pins both, because the point is that they agree.
#[test]
fn a_failed_write_leaves_no_local_shadow_behind() {
    let member = run_with_input(
        "m = [a: 1]\n\
         func f() { $m.missing.deep = 2\n\
         global $m.a = 3\n\
         puts sees $m.a }\n\
         f\n\
         puts global $m.a\n",
    );
    // Without the fix the failed write shadows `m`, so `sees` reports the stale 1.
    assert_eq!(
        String::from_utf8_lossy(&member.stdout),
        "sees 3\nglobal 3\n"
    );
    assert!(
        String::from_utf8_lossy(&member.stderr).contains("no `missing` in this map"),
        "{}",
        String::from_utf8_lossy(&member.stderr)
    );

    let whole = run_with_input(
        "n = [a: 1]\n\
         func f() { n += 5\n\
         global n = [a: 3]\n\
         puts sees $n.a }\n\
         f\n",
    );
    assert_eq!(String::from_utf8_lossy(&whole.stdout), "sees 3\n");

    // The shadow still appears when the write *succeeds*: that is the
    // local-by-default rule, not a leak.
    let succeeded = run_with_input(
        "m = [a: 1]\n\
         func f() { $m.a = 2\n\
         global $m.a = 3\n\
         puts sees $m.a }\n\
         f\n\
         puts global $m.a\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&succeeded.stdout),
        "sees 2\nglobal 3\n"
    );
}

/// `unset $m.key` and `unset $xs[0]` remove an **entry** rather than the binding
/// that holds it, the deletion `TODO.md` recorded as waiting on member assignment.
/// It shares that feature's path walker, so a nested path, a negative index, and a
/// quoted key all behave as they do on the way in.
#[test]
fn unset_removes_a_collection_element() {
    let out = run_with_input(
        "m = [a: 1, b: 2, c: 3]\n\
         unset $m.b\n\
         puts ...$m:keys\n\
         xs = [10 20 30]\n\
         unset $xs[1]\n\
         puts ...$xs\n\
         unset $xs[-1]\n\
         puts ...$xs\n\
         nested = [outer: [x: 1, y: 2]]\n\
         unset $nested.outer.x\n\
         puts ...$nested.outer:keys\n\
         keyed = [\"a:b\": 1, other: 2]\n\
         unset $keyed[\"a:b\"]\n\
         puts ...$keyed:keys\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // Removing from a list shifts what follows, so `unset $xs[1]` drops the
        // element rather than leaving a hole.
        "a c\n10 30\n10\ny\nother\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Names and places mix in one statement, each handled its own way.
    let mixed = run_with_input("p = 1\nq = 2\nm = [k: 9]\nunset p $m.k q\nputs $m:len\nputs $p\n");
    assert_eq!(String::from_utf8_lossy(&mixed.stdout), "0\n");
    assert!(
        String::from_utf8_lossy(&mixed.stderr).contains("p: unbound variable"),
        "{}",
        String::from_utf8_lossy(&mixed.stderr)
    );
}

/// Removing an element follows the same scope rule writing one does — local by
/// default, `global` to reach the outer binding — and a **failed** removal leaves
/// no local shadow behind, both of which come from sharing `Vars::update`.
#[test]
fn unsetting_an_element_follows_the_member_assignment_scope_rules() {
    let out = run_with_input(
        "m = [a: 1, b: 2]\n\
         func local() { unset $m.a\n\
         puts inside $m:len }\n\
         local\n\
         puts outside $m:len\n\
         func through() { global unset $m.a\n\
         puts g-inside $m:len }\n\
         through\n\
         puts g-outside $m:len\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // The local removal shadows; `global unset` carries out of the function.
        "inside 1\noutside 2\ng-inside 1\ng-outside 1\n"
    );

    // A failed removal must not shadow either — otherwise the later `global`
    // write would be read past by a stale copy.
    let failed = run_with_input(
        "k = [a: 1]\n\
         func f() { unset $k.nope\n\
         global $k.a = 3\n\
         puts sees $k.a }\n\
         f\n",
    );
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "sees 3\n");
}

/// The same fail-loud rules the assignment side uses, in `unset`'s own words.
#[test]
fn unsetting_a_missing_element_is_a_loud_error() {
    for (source, message) in [
        (
            "m = [a: 1]\nunset $m.nope\n",
            "$m.nope: no `nope` in this map",
        ),
        (
            "xs = [1 2]\nunset $xs[9]\n",
            "$xs[9]: list index out of range",
        ),
        (
            "xs = [1 2 3]\nunset $xs[0..2]\n",
            "$xs[0..2]: cannot unset a slice",
        ),
        (
            "s = \"t\"\nunset $s.k\n",
            "$s.k: cannot unset from a string",
        ),
        (
            "xs = [1 2]\nunset $xs.k\n",
            "$xs.k: a list has no `k` member",
        ),
        ("unset $nope.k\n", "nope: unbound variable"),
    ] {
        let out = run_with_input(source);
        assert!(!out.status.success(), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }

    // `$env` is not a place here, so removing an entry from the environment is
    // still not spelled this way.
    let out = run_with_input("unset $env.PATH\n");
    assert_eq!(out.status.code(), Some(2));

    // `$sh` parses as one, and is refused by name instead — including the
    // settings, which are writable but not removable.
    for (source, message) in [
        ("unset $sh.pid\n", "`$sh.pid` is read-only"),
        (
            "unset $sh.options.bold-input\n",
            "a setting cannot be removed",
        ),
        ("unset $sh.options\n", "`$sh.options` cannot be removed"),
    ] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(1), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }
}

/// A colon inside a **subscript** belongs to the key, not to a modifier chain, so
/// every quoted key that reads also writes. Scanning the target text for a bare `:`
/// got this wrong: `$m["a:b"]` reads fine but was rejected as an assignment target.
#[test]
fn a_key_containing_a_colon_is_writable_as_well_as_readable() {
    let out = run_with_input(
        "m = [\"a:b\": 1]\n\
         puts read $m[\"a:b\"]\n\
         $m[\"a:b\"] = 9\n\
         puts wrote $m[\"a:b\"]\n\
         nested = [\"a:b\": [x: 1]]\n\
         $nested[\"a:b\"].x = 5\n\
         puts deep $nested[\"a:b\"].x\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "read 1\nwrote 9\ndeep 5\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A `:` *between* accesses is still a modifier, and still not a place.
    for source in [
        "xs = [1 2]\n$xs:dedup = 9\n",
        "m = [a: 1]\n$m.a:upper = X\n",
    ] {
        let rejected = run_with_input(source);
        assert_eq!(rejected.status.code(), Some(2), "{source}");
    }
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
        "func f() {\n  i = 0\n  while $i < 9 { i = $i + 1\n    if $i == 3 { fail 7 } }\n  puts unreachable\n}\nf\n",
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

/// A **numeral** operand gets the condition's comparison reading too, and needed
/// saying separately: a variable or a quoted word is claimed by the clause that takes
/// the whole statement once no redirect follows, so ruling the redirect out is enough
/// for them. A numeral is claimed only by a leading operator or by being a lone
/// literal, and `if 1 < 2` is neither — so it reached the command parser, failed to
/// open a file named `2`, and took the **else** branch, answering a comparison with
/// its opposite. `if 1 == 1` beside it compared, since `==` has no redirect spelling.
///
/// `-1:repr:len` is `-(1:repr:len)`, i.e. `-1`, since unary minus binds looser than
/// postfix — so it is compared with `< 0` rather than `> 0`. What matters here is that
/// it *compares at all* instead of reaching the command parser.
///
/// The sign needs no space rule of its own. A leading `-` is unary minus in mesh with
/// or without a space after it — `x = - 3` is −3 the same as `x = -3` — so `if - 1 < 0`
/// compares like `if -1 < 0` rather than opening a file named `0`.
///
/// In a scratch directory because the `>` forms create the file they compare against.
#[test]
fn a_numeral_in_a_condition_compares_rather_than_redirecting() {
    let dir = fresh_dir("numeral_condition_comparison");
    std::fs::write(
        dir.join("run.mesh"),
        "if 1 < 2 { puts int } else { puts wrong }\n\
         if 1 > 0 { puts int-gt } else { puts wrong }\n\
         if -1 < 0 { puts signed } else { puts wrong }\n\
         if - 1 < 0 { puts spaced-sign } else { puts wrong }\n\
         if -9223372036854775808 < 0 { puts min } else { puts wrong }\n\
         if -9223372036854775807 < 0 { puts near-min } else { puts wrong }\n\
         if 1:repr:len > 0 { puts modified } else { puts wrong }\n\
         if -1:repr:len < 0 { puts signed-modified } else { puts wrong }\n\
         if 1 == 1 { puts eq } else { puts wrong }\n",
    )
    .unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "int\nint-gt\nsigned\nspaced-sign\nmin\nnear-min\nmodified\nsigned-modified\neq\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for name in ["0", "2"] {
        assert!(
            !dir.join(name).exists(),
            "a condition comparison redirected into {name:?}"
        );
    }

    // The boundary is "fits an `i64`" on both signs, so an out-of-range literal is not
    // a numeral and keeps the command reading it had.
    std::fs::write(
        dir.join("run.mesh"),
        "if -9223372036854775809 < 0 { puts x }\n",
    )
    .unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert!(
        !String::from_utf8_lossy(&out.stderr).is_empty(),
        "an out-of-range literal should stay a command"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A modifier that takes **arguments** moves the operator past a `(`, and no token
/// scan can follow it there — a command word stops in front of `(`, which is exactly
/// why the scan does. So `if 1:repr:split("x"):len > 0` found no comparison, reached
/// the command parser, and reported "expected a command word" for a line whose
/// argument-free spelling on the line above it worked.
///
/// The fix is to stop scanning for the operator and ask the parse where it landed,
/// which is why this holds for every chain rather than for the two lengths below.
#[test]
fn a_modifier_taking_arguments_still_leaves_a_comparison() {
    let out = run_with_input(
        "if 1:repr:split(\"x\"):len > 0 { puts args } else { puts wrong }\n\
         if -1:repr:split(\"1\"):len < 9 { puts signed-args } else { puts wrong }\n\
         x = abc\n\
         if $x:split(\"b\"):len > 1 { puts var-args } else { puts wrong }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "args\nsigned-args\nvar-args\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

/// Reading the statement as a value means *parsing* it, and a command line parses as
/// an expression more often than it looks: `ls / extra` is a division and `exit -1` a
/// subtraction. What tells them apart from `1 == 2` is not the shape of the tree but
/// its **leading operand** — a bare word is what a command is spelled with, so an
/// expression leading with one stays the command line it is.
#[test]
fn a_value_expression_can_be_a_command_argument() {
    // `DESIGN.md` writes two of these in its own examples — `puts (1 + 2)` in
    // §"Arithmetic" and `puts $(ls)` in §"I/O" — and every one was a syntax error.
    let out = run_with_input(
        "puts (1 + 2)\n\
         puts (10 / 3)\n\
         puts a (1 + 2) b\n\
         n = 4\n\
         puts ($n + 3)\n\
         puts $(pwd)\n\
         puts before $(pwd) after\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(&lines[..4], &["3", "3", "a 3 b", "7"], "{stdout:?}");
    // The capture reaches argv rather than being refused, and glues to neighbours in
    // argument order.
    assert!(lines[4].starts_with('/'), "{stdout:?}");
    assert_eq!(lines[5], format!("before {} after", lines[4]), "{stdout:?}");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
}

#[test]
fn a_value_argument_stays_typed_for_a_builtin_and_bytes_for_an_external() {
    // The argument carries the **value**, not its text, so everything the styled and
    // collection work built still applies at this new position: `puts` renders a list
    // per line and keeps a styled value's attributes, while argv gets the same loud
    // refusal it gives a bare list.
    let rendered =
        run_with_input("func xs() { return [a b] }\nputs xs()\nputs style(x, fg: red)\n");
    assert_eq!(String::from_utf8_lossy(&rendered.stdout), "a\nb\nx\n");
    assert!(rendered.stderr.is_empty(), "{:?}", rendered.stderr);

    let refused = run_with_input("func xs() { return [a b] }\n/bin/echo xs()\nputs after\n");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("a list needs `...`"),
        "{:?}",
        refused.stderr
    );
    assert_eq!(String::from_utf8_lossy(&refused.stdout), "after\n");

    // A value with no byte form is refused by name wherever it lands.
    let handle = run_with_input("puts re(a)\nputs after\n");
    assert!(
        String::from_utf8_lossy(&handle.stderr).contains("a pattern has no text form"),
        "{:?}",
        handle.stderr
    );
    assert_eq!(String::from_utf8_lossy(&handle.stdout), "after\n");
}

#[test]
fn a_value_argument_needs_a_command_word_before_it() {
    // A value is an *argument*, so it cannot be word zero. A leading redirection makes
    // the item list non-empty without naming a command, so testing emptiness let
    // `>out f()` take the call's value as the command and run it — where these were
    // syntax errors before value arguments existed.
    let dir = fresh_dir("value_argument_needs_a_word");
    let file = dir.join("out");
    for source in ["func f() { return /bin/echo }\n>{} f()\n", ">{} (1 + 2)\n"] {
        let _ = std::fs::remove_file(&file);
        let out = run_with_input(&source.replace("{}", &file.display().to_string()));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{source:?}: {:?}",
            out.stderr
        );
    }

    // A redirection may still *lead*, as long as a command word precedes the value.
    let led = run_with_input(&format!(">{} puts (1 + 2)\n", file.display()));
    assert!(led.stderr.is_empty(), "{:?}", led.stderr);
    assert_eq!(
        std::fs::read_to_string(&file).ok().as_deref(),
        Some("3\n"),
        "a leading redirect should not stop a later value argument"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stage_evaluates_its_own_value_arguments_in_its_fork() {
    // A value used to be evaluated while the stage was assembled, before the fork, so
    // the work landed in the shell: `puts $(sleep …) &` was refused outright because
    // it would hang the prompt, and a mutating call in a *piped* stage reached the
    // parent's bindings where `docs/REFERENCE.md` says the fork keeps them.

    // Backgrounding one is ordinary now, and the claim `&` makes is about the
    // **shell**: it does not wait. Which process reaches stdout first is a race the
    // background child can legitimately win, so this measures the shell instead —
    // one that evaluated a five-second value itself could not finish the script in
    // under five seconds, and one that hands it to the job finishes at once.
    for source in ["puts $(/bin/sleep 5) &\n", "puts \"[$(/bin/sleep 5)]\" &\n"] {
        let start = std::time::Instant::now();
        let backgrounded = run_with_input(&format!("{source}puts after\n"));
        let elapsed = start.elapsed();
        // Stderr carries the `[1] <pid>` registration notice, and nothing else: the
        // command was accepted rather than refused.
        let stderr = String::from_utf8_lossy(&backgrounded.stderr);
        assert!(stderr.starts_with("[1] "), "{source:?}: {stderr:?}");
        assert!(!stderr.contains("mesh:"), "{source:?}: {stderr:?}");
        assert_eq!(String::from_utf8_lossy(&backgrounded.stdout), "after\n");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "{source:?}: the shell waited {elapsed:?} for a backgrounded value"
        );
    }

    // The isolation the deferral is really about, on both spellings that fork. Piped
    // left `n=MUTATED` before; backgrounded was refused rather than wrong.
    for source in ["puts change() | cat\n", "puts change() &\nwait\n"] {
        let isolated = run_with_input(&format!(
            "n = before\nfunc change() {{ global n = MUTATED\n  return x }}\n{source}puts n=$n\n"
        ));
        let stdout = String::from_utf8_lossy(&isolated.stdout);
        assert!(stdout.contains('x'), "{source:?}: {stdout:?}");
        assert!(stdout.contains("n=before"), "{source:?}: {stdout:?}");
    }

    // A stage that **redirects** cannot defer: the shell resolves every stage's
    // targets before it forks any of them, in parallel, so deferring the words alone
    // would put the targets first. Backgrounding one stays refused — whether the
    // value is in a word or in the target itself.
    for source in ["puts hi > \"o$(pwd).txt\" &\n", "puts $(pwd) > out.txt &\n"] {
        let refused = run_with_input(&format!("{source}puts after\n"));
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("a value cannot be backgrounded with a redirection yet"),
            "{source:?}: {:?}",
            refused.stderr
        );
        assert_eq!(String::from_utf8_lossy(&refused.stdout), "after\n");
    }

    // Each kind of command a deferred stage can turn out to be, since the kind is not
    // known until the fork expands the words: an external to `exec`, a builtin, a
    // function — and an external that does not exist, still 127 with the same message.
    let kinds = run_with_input(
        "func f(x) { puts f=$x }\n\
         /bin/echo $(puts ext) | cat\n\
         puts $(puts builtin) | cat\n\
         f $(puts fn) | cat\n\
         nosuchcmd $(pwd) | cat\n\
         puts status=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&kinds.stdout),
        "ext\nbuiltin\nf=fn\nstatus=127\n",
        "{:?}",
        kinds.stderr
    );
    assert!(
        String::from_utf8_lossy(&kinds.stderr).contains("command not found: nosuchcmd"),
        "{:?}",
        kinds.stderr
    );

    // What a stage cannot be is still refused in the same terms, now that the words
    // deciding it are only known in the fork. `return` unwinds a function; reaching it
    // through a variable is the one way it can land in a stage.
    let piped_return = run_with_input("c = return\nputs hi | $c $(puts 3)\nputs s=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&piped_return.stderr)
            .contains("return: cannot be used in a pipeline"),
        "{:?}",
        piped_return.stderr
    );
    assert!(String::from_utf8_lossy(&piped_return.stdout).contains("s=2"));

    // Backgrounded, the refusal comes from the job rather than the prompt — the same
    // shape any failing background command has (`nosuchcmd &` starts and reports 127).
    let background_return = run_with_input("c = return\n$c $(puts 3) &\nwait\nputs s=$sh.status\n");
    assert!(
        String::from_utf8_lossy(&background_return.stderr)
            .contains("return: cannot be redirected or backgrounded"),
        "{:?}",
        background_return.stderr
    );
    assert!(String::from_utf8_lossy(&background_return.stdout).contains("s=2"));
}

#[test]
fn a_value_in_a_word_is_evaluated_when_that_word_is_expanded() {
    // Evaluating every value up front made a call in one argument visible to words
    // written *earlier* on the line: `$cmd` was expanded after `g()` had already
    // reassigned it, so this ran `/bin/false` and printed nothing.
    let selected = run_with_input(
        "cmd = /bin/echo\n\
         func g() { global cmd = /bin/false\n  return x }\n\
         $cmd g()\n\
         puts status=$sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&selected.stdout),
        "x\nstatus=0\n",
        "{:?}",
        selected.stderr
    );

    // The general rule, both directions at once: the word before the call reads the
    // old value and the word after it reads the new one. Up-front evaluation printed
    // `second x second`.
    let both =
        run_with_input("n = first\nfunc g() { global n = second\n  return x }\nputs $n g() $n\n");
    assert_eq!(String::from_utf8_lossy(&both.stdout), "first x second\n");

    // Word zero is expanded **once**, for all that it is asked several questions —
    // is it a function, is it a typed builtin, what is its argv entry. A value in it
    // must not run again for each, so the counter reads 1 on all three paths word
    // zero can name.
    for (names, want) in [
        ("/bin/echo", "hi\nran=1\n"),
        ("puts", "hi\nran=1\n"),
        ("f", "f=hi\nran=1\n"),
    ] {
        let out = run_with_input(&format!(
            "n = 0\nfunc f(x) {{ puts f=$x }}\n\
             func pick() {{ global n = ($n + 1)\n  puts {names} }}\n\
             \"$(pick)\" hi\nputs ran=$n\n"
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{names}: {:?}",
            out.stderr
        );
    }
}

#[test]
fn a_redirect_target_is_expanded_after_every_word() {
    // The documented order, which a value in a target has to follow too: it used to
    // be evaluated while the stage was assembled, so `puts $n > "$(g)"` wrote the
    // value `g` had just assigned rather than the one written on the line.
    let dir = fresh_dir("redirect_target_after_words");
    let target = dir.join("out");
    let out = run_with_input(&format!(
        "n = first\nfunc g() {{ global n = second\n  puts {} }}\nputs $n > \"$(g)\"\n",
        target.display()
    ));
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(&target).ok().as_deref(),
        Some("first\n")
    );

    // And the target comes after a value *argument*, which is what keeps
    // `f * > summary` from globbing the file the redirection is about to create.
    let order = run_with_input(&format!(
        "order = ''\n\
         func arg() {{ global order = \"${{order}}arg-\"\n  return x }}\n\
         func tgt() {{ global order = \"${{order}}tgt\"\n  puts {} }}\n\
         /bin/echo arg() > \"$(tgt)\"\nputs order=$order\n",
        target.display()
    ));
    assert_eq!(String::from_utf8_lossy(&order.stdout), "order=arg-tgt\n");
    assert_eq!(
        std::fs::read_to_string(&target).ok().as_deref(),
        Some("x\n")
    );

    // The order holds for a **piped** stage carrying a value too, which is what stops
    // such a stage deferring its words to its own fork: deferring the words alone
    // would leave the target expanded first, and then a failing one stopped the words
    // from running at all and a glob matched the file its own redirection created.
    let globbed = fresh_dir("redirect_target_after_words_piped");
    std::fs::write(globbed.join("a1"), "").unwrap();
    std::fs::write(globbed.join("a2"), "").unwrap();
    let counted = run_with_input(&format!(
        "cd {}\nfunc f(...xs) {{ puts got=$xs:len }}\nf * $(puts x) > summary | cat\n",
        globbed.display()
    ));
    // `a1`, `a2` and the value — not the `summary` the redirection creates.
    assert_eq!(String::from_utf8_lossy(&counted.stdout), "");
    assert_eq!(
        std::fs::read_to_string(globbed.join("summary"))
            .ok()
            .as_deref(),
        Some("got=3\n"),
        "{:?}",
        counted.stderr
    );

    // And a failing target does not stop the words from running.
    let failing = run_with_input(
        "func g() { puts G-RAN\n  return x }\nputs g() > $missing | cat\nputs s=$sh.status\n",
    );
    let stdout = String::from_utf8_lossy(&failing.stdout);
    assert!(stdout.contains("G-RAN"), "{stdout:?}");
    assert!(stdout.contains("s=1"), "{stdout:?}");

    let _ = std::fs::remove_dir_all(&globbed);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_deferred_stage_does_not_reap_a_finished_job_out_from_under_a_later_wait() {
    // A stage whose words are expanded in its own fork could turn out to be `jobs`,
    // which needs a refreshed table — but almost every one of them is an ordinary
    // command, and reaping for those takes a finished job out from under a later
    // `fg`/`wait`, the very cost the refresh is kept conditional to avoid. So the
    // command *word* is asked, before anything is expanded.
    let ordinary = run_with_input(
        "j = /bin/sh -c \"exit 7\" &\n\
         /bin/sleep 0.3\n\
         puts $(puts hi) | cat\n\
         wait $j\n\
         puts s=$sh.status\n",
    );
    let stdout = String::from_utf8_lossy(&ordinary.stdout);
    assert!(stdout.contains("hi"), "{stdout:?}");
    // The handle still names the job, and `wait` gives back its status. Reaping at
    // the deferred stage left `mesh: wait: no current job` and a status of 1.
    assert!(stdout.contains("s=7"), "{stdout:?}");
    assert!(
        !String::from_utf8_lossy(&ordinary.stderr).contains("no current job"),
        "{:?}",
        ordinary.stderr
    );

    // A deferred stage that *is* `jobs` still refreshes: the command word says so
    // without anything being expanded.
    let listing = run_with_input(
        "/bin/sh -c \"exit 7\" &\n/bin/sleep 0.3\njobs $(puts x) | cat\nputs after\n",
    );
    assert!(
        String::from_utf8_lossy(&listing.stderr).contains("Done"),
        "{:?}",
        listing.stderr
    );
}

#[test]
fn control_flow_inside_a_value_argument_belongs_to_the_caller() {
    // The argument runs with the caller's `in_function`, so a `return` in one leaves
    // the enclosing function. Evaluated as top-level code it reported "not inside a
    // function" and the body carried on past it.
    let returned = run_with_input(
        "func f() { puts (if true { return 7 })\n  puts BAD }\nx = f()\nputs x=$x:repr\n",
    );
    assert_eq!(String::from_utf8_lossy(&returned.stdout), "x=7\n");
    assert!(returned.stderr.is_empty(), "{:?}", returned.stderr);

    // A `break` in an argument still belongs to the enclosing loop.
    let broken = run_with_input(
        "for i in [1 2 3] { puts (if $i == 2 { break })\n  puts saw=$i }\nputs done\n",
    );
    let stdout = String::from_utf8_lossy(&broken.stdout);
    assert!(stdout.contains("saw=1"), "{stdout:?}");
    assert!(!stdout.contains("saw=2"), "{stdout:?}");
    assert!(stdout.ends_with("done\n"), "{stdout:?}");
}

#[test]
fn a_redirect_after_a_value_argument_still_redirects() {
    // A command argument is parsed just *above* comparison precedence, so a following
    // `<` / `>` is left to the redirect parser. Read as a comparison instead, these
    // printed `true` (or failed comparing) and created no file.
    let dir = fresh_dir("value_argument_redirect");
    for (source, want) in [
        ("puts (1 + 2) > {}\n", "3\n"),
        ("puts $(puts hi) > {}\n", "hi\n"),
        ("puts style(x, fg: red) > {}\n", "x\n"),
    ] {
        let file = dir.join("out");
        let _ = std::fs::remove_file(&file);
        let out = run_with_input(&source.replace("{}", &file.display().to_string()));
        assert!(out.stdout.is_empty(), "{source:?}: {:?}", out.stdout);
        assert!(out.stderr.is_empty(), "{source:?}: {:?}", out.stderr);
        assert_eq!(
            std::fs::read_to_string(&file).ok().as_deref(),
            Some(want),
            "{source:?}"
        );
    }

    // A comparison that really is wanted says so with its own parens, where a fresh
    // expression parse gives it back.
    let compared = run_with_input("puts (1 < 2)\nputs (2 <= 1)\n");
    assert_eq!(String::from_utf8_lossy(&compared.stdout), "true\nfalse\n");

    // The connectives sit below comparison too, so they keep their readings.
    let connected = run_with_input("func f() { return v }\nputs f() && puts second\n");
    assert_eq!(String::from_utf8_lossy(&connected.stdout), "v\nsecond\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_glued_to_a_value_argument_is_a_loud_error() {
    // A value argument is a whole argument. Silently handing over three arguments
    // where `pre$(x)post` was written would be worse than the syntax error this was
    // before value arguments existed, so it stays one — with a message naming the
    // quoted spelling, which does interpolate.
    for source in [
        "/bin/echo pre$(puts x)post\n",
        "puts f()x\n",
        "puts x$(puts y)\n",
        // A **redirect target** counts too: `>out$(x)` reaches the check with
        // `Redirect` as the last item, so a word-only test let it through and passed
        // the capture as a separate argument.
        "/bin/echo hi >out$(puts suffix)\n",
    ] {
        let out = run_with_input(&format!("func f() {{ return v }}\n{source}"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("a value argument cannot have text attached"),
            "{source:?}: {stderr:?}"
        );
        assert!(out.stdout.is_empty(), "{source:?}: {:?}", out.stdout);
    }

    // The spelling the message points at does work.
    let quoted = run_with_input("/bin/echo \"pre$(puts x)post\"\n");
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "prexpost\n");
    assert!(quoted.stderr.is_empty(), "{:?}", quoted.stderr);

    // And a value argument flush against the *end* of a command is not glued text —
    // a newline sits there too.
    let trailing = run_with_input("func f() { return v }\nputs f()\nputs f();\n");
    assert_eq!(String::from_utf8_lossy(&trailing.stdout), "v\nv\n");
    assert!(trailing.stderr.is_empty(), "{:?}", trailing.stderr);
}

#[test]
fn a_value_argument_is_literal_and_leaves_globs_alone() {
    let dir = fresh_dir("value_argument_literal");
    std::fs::write(dir.join("apple"), "").unwrap();
    std::fs::write(dir.join("banana"), "").unwrap();
    let listing = dir.display();

    // `[` and `..` keep their argument readings: a glob character class and the
    // literal word. Reading either as value syntax here would break working scripts,
    // so only the shapes with *no* word spelling became values.
    let unchanged = run_with_input(&format!("puts 1..3\nputs {listing}/[ab]*\n"));
    assert_eq!(
        String::from_utf8_lossy(&unchanged.stdout),
        format!("1..3\n{listing}/apple {listing}/banana\n")
    );

    // And a value argument is literal, exactly as an interpolated variable is: what a
    // capture produced is never re-globbed.
    let literal = run_with_input(&format!("cd {listing}\nputs $(puts '*')\n"));
    assert_eq!(String::from_utf8_lossy(&literal.stdout), "*\n");
    assert!(literal.stderr.is_empty(), "{:?}", literal.stderr);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_capture_interpolates_inside_double_quotes() {
    // `"… $(cmd) …"` printed its own source text before: the piece scanner read the
    // `$` as a literal and the `(` as more text. `DESIGN.md` writes the prompt idiom
    // `style("$(hostname)", fg: red)` in §"Prompt", which yielded the *string*
    // `$(hostname)`.
    let out = run_with_input(
        "puts \"at $(puts here) now\"\n\
         puts \"$(puts one)\"\n\
         puts \"$(puts a) and $(puts b)\"\n\
         m = \"pre$(puts x)post\"\n\
         puts $m\n\
         puts (style(\"$(puts host)\", fg: red))\n\
         func f(n) { puts \"got $n from $(puts cap)\" }\n\
         f 1\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "at here now\none\na and b\nprexpost\nhost\ngot 1 from cap\n"
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // Only in double quotes. A single-quoted or raw string is literal, and `\$(` is
    // the escape that keeps the text where a double-quoted string is wanted.
    let literal = run_with_input(
        "puts '$(puts no)'\n\
         puts r\"$(puts no)\"\n\
         puts \"\\$(puts no)\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&literal.stdout),
        "$(puts no)\n$(puts no)\n$(puts no)\n"
    );
    assert!(literal.stderr.is_empty(), "{:?}", literal.stderr);
}

#[test]
fn an_interpolated_capture_crosses_whole() {
    // Quoted, so the output is one argument however many spaces and glob characters
    // it contains — the rule an interpolated variable already follows. A shell that
    // re-split here is where `for f in "$(ls)"` goes wrong.
    let dir = fresh_dir("interpolated_capture_whole");
    std::fs::write(dir.join("apple"), "").unwrap();
    std::fs::write(dir.join("banana"), "").unwrap();

    let out = run_with_input(&format!(
        "cd {}\n\
         x = \"$(puts 'a b')\"\n\
         puts $x:len\n\
         puts \"$(puts '*')\"\n\
         for w in \"$(puts one)\" {{ puts item=$w }}\n",
        dir.display()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3\n*\nitem=one\n",
        "{:?}",
        out.stderr
    );
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_interpolated_capture_yields_its_output_whatever_the_status() {
    // A capture's bytes are the answer whatever the command exited with, so an
    // interpolated one substitutes rather than stopping the statement. `false`
    // prints nothing, so the interpolation is empty — but the statement runs.
    let failed = run_with_input("puts \"[$(false)]\"\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "[]\nafter\n");

    // The output survives even when the command that produced it failed, which is
    // the case `diff` and friends depend on. The status here is the `puts`'s own,
    // as it is in bash — an interpolating command is not an assignment.
    let partial = run_with_input("puts \"[$(sh -c 'echo kept; exit 3')]\"\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&partial.stdout), "[kept]\nafter\n");

    // A syntax error inside the capture is reported at *parse* time, so nothing in
    // the statement runs.
    let malformed = run_with_input("puts \"[$(if)]\"\nputs after\n");
    assert!(
        String::from_utf8_lossy(&malformed.stderr).contains("syntax error"),
        "{:?}",
        malformed.stderr
    );
    assert_eq!(String::from_utf8_lossy(&malformed.stdout), "after\n");

    // An unclosed capture is a syntax error naming the delimiter, not a hang.
    let unclosed = run_with_input("puts \"[$(pwd\n");
    assert!(
        String::from_utf8_lossy(&unclosed.stderr).contains("unclosed `(`"),
        "{:?}",
        unclosed.stderr
    );
}

#[test]
fn a_heredoc_keeps_a_capture_out_of_its_delimiter_and_its_body() {
    // A body is interpolated from its text, and only for `$…` references — so a
    // capture stays as written. `docs/REFERENCE.md` §"Heredocs" says so.
    let body = run_with_input("x = X\ncat << END\nvar $x and $(puts cap)\nEND\n");
    assert_eq!(
        String::from_utf8_lossy(&body.stdout),
        "var X and $(puts cap)\n",
        "{:?}",
        body.stderr
    );

    // A **delimiter** is matched as text, so a capture in one would mean running a
    // command to decide where the body ends. Refused rather than run: this used to be
    // the literal delimiter `$(x)`, which is nobody's intent worth keeping.
    let delimiter = run_with_input("cat <<\"$(puts x)\"\nbody\n$(puts x)\n");
    assert!(
        String::from_utf8_lossy(&delimiter.stderr)
            .contains("a heredoc delimiter without a capture"),
        "{:?}",
        delimiter.stderr
    );
    assert!(delimiter.stdout.is_empty(), "{:?}", delimiter.stdout);

    // An ordinary quoted delimiter is untouched.
    let quoted = run_with_input("cat << \"END\"\nliteral $x\nEND\n");
    assert_eq!(String::from_utf8_lossy(&quoted.stdout), "literal $x\n");
}

#[test]
fn a_command_line_that_parses_as_an_expression_is_still_a_command() {
    let dir = fresh_dir("command_line_parses_as_expression");
    // Infix operators between a command and its arguments.
    let divided = run_with_input(&format!("cd {}\nls / extra\n", dir.display()));
    assert!(
        String::from_utf8_lossy(&divided.stderr).contains("extra"),
        "`ls / extra` should reach ls: {:?}",
        divided.stderr
    );
    assert_eq!(run_with_input("exit -1\n").status.code(), Some(255));
    assert!(
        String::from_utf8_lossy(&run_with_input("cd / extra\n").stderr).contains("too many"),
        "`cd / extra` should reach cd"
    );
    // `..` is a range operator between values and a directory name after a command.
    let parent = run_with_input(&format!("cd {}\nls ..\n", dir.display()));
    assert!(parent.stderr.is_empty(), "`ls ..` should list: {parent:?}");

    // A *spaced* `(` is the next argument, an **attached** one is a call, and the tree
    // does not record the difference — so the check for it happens before the parse.
    // Both sides are asserted here, since a spacing rule with only one side tested is
    // a rule that can quietly collapse.
    let spaced = run_with_input("puts (1 + 2)\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&spaced.stdout), "3\nafter\n");
    assert!(spaced.stderr.is_empty(), "{:?}", spaced.stderr);

    let attached = run_with_input("r = puts(1 + 2)\nputs after\n");
    assert!(
        String::from_utf8_lossy(&attached.stderr).contains("a command has no return value"),
        "attached `(` should be a call on the command: {:?}",
        attached.stderr
    );

    // Only a **command word** loses to a following `&&` / `||` / `&`, because only it
    // has the command reading the shell idiom wants — see
    // `a_value_that_is_no_command_word_keeps_its_connectors`.
    let variable = run_with_input("cmd = nosuchcmd\n$cmd || puts fallback\n");
    assert!(
        String::from_utf8_lossy(&variable.stderr).contains("command not found: nosuchcmd"),
        "{:?}",
        variable.stderr
    );
    assert_eq!(String::from_utf8_lossy(&variable.stdout), "fallback\n");
    let compared = run_with_input("1 == 2 || puts ok\n1 == 1 && puts also\n");
    assert_eq!(String::from_utf8_lossy(&compared.stdout), "ok\nalso\n");
    assert!(compared.stderr.is_empty(), "{:?}", compared.stderr);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `$cmd || puts failed` is the shell idiom, so a value that *is* a command word loses
/// to a following `&&` / `||` / `&`. The bound was drawn at "a **variable** leads the
/// expression", which handed the command reading to text that has none — and the
/// connector then picked a reading that could not work:
///
/// ```text
/// $a == $b && puts eq        # ran the command `5`
/// $x ~ /b/ && puts matched   # ran the command `abc`
/// $x:split("-") || puts x    # syntax error: a command word stops in front of `(`
/// ```
///
/// `1 == 2 || puts no` compared throughout, since a numeral leads it — which is what
/// made the variable cases read as arbitrary rather than as a rule.
///
/// What separates the two is **whitespace**, not shape. A command word is an unbroken
/// run of tokens, and `${cmd}.exe`, `${cmd}[0]`, `${cmd}..bak`, and `${cmd}-1` are each
/// one word naming a program while the tree calls them a member access, an index, a
/// range, and a subtraction — each indistinguishable from the spaced expression of the
/// same shape, since `$a - 1` really is arithmetic. Only `(` is ruled out by shape,
/// command position having no call syntax.
#[test]
fn a_value_that_is_no_command_word_keeps_its_connectors() {
    // A comparison, a match, and arithmetic on a variable all report their own status.
    let out = run_with_input(
        "a = 5\nb = 6\n\
         $a == $b || puts ne\n\
         $a != $b && puts also-ne\n\
         $a >= $b || puts lt\n\
         x = abc\n\
         $x ~ /b/ && puts matched\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ne\nalso-ne\nlt\nmatched\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A modifier taking **arguments** reaches past where a command word can end, so
    // this was a syntax error about a missing command word before.
    let wide = run_with_input("x = a-b-c\n$x:split(\"-\"):len == 3 && puts three\n");
    assert_eq!(
        String::from_utf8_lossy(&wide.stdout),
        "three\n",
        "{}",
        String::from_utf8_lossy(&wide.stderr)
    );
    assert!(
        wide.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&wide.stderr)
    );

    // Every **attached** suffix is command-word text, whatever the tree calls it. Each
    // of these is one word naming a program, and each was sent to a value operation —
    // member access, indexing, a string range, a subtraction — when the rule was drawn
    // by shape instead of by whitespace.
    let dir = fresh_dir("braced_command_word_suffix");
    for name in ["tool.exe", "tool..bak", "tool-1", "tool0"] {
        let script = dir.join(name);
        std::fs::write(&script, format!("#!/bin/sh\necho ran-{name}\nexit 3\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
    }
    for (suffix, ran) in [
        (".exe", "ran-tool.exe"),
        ("..bak", "ran-tool..bak"),
        ("-1", "ran-tool-1"),
    ] {
        let out = run_with_input(&format!(
            "cd {}\ncmd = \"./tool\"\n${{cmd}}{suffix} || puts fallback\n",
            dir.display()
        ));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{ran}\nfallback\n"),
            "{suffix} is command-word text: {:?}",
            out.stderr
        );
    }

    // `[0]` globs rather than indexing, so it has to match a real name to expand at
    // all — and the glob drops the leading `./`, which is why this asserts on the
    // lookup rather than on a run. An index reading says "cannot index a scalar value"
    // and never looks for a program.
    let bracketed = run_with_input(&format!(
        "cd {}\ncmd = \"./tool\"\n${{cmd}}[0] || puts fallback\n",
        dir.display()
    ));
    assert!(
        String::from_utf8_lossy(&bracketed.stderr).contains("command not found: tool0"),
        "a braced command word globs its bracket suffix: {:?}",
        bracketed.stderr
    );
    assert_eq!(String::from_utf8_lossy(&bracketed.stdout), "fallback\n");
    let backgrounded_word = run_with_input("cmd = \"./nosuch\"\n${cmd}.exe &\nputs after\n");
    assert!(
        !String::from_utf8_lossy(&backgrounded_word.stderr).contains("backgrounding an expression"),
        "a command word backgrounds rather than being refused: {:?}",
        backgrounded_word.stderr
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Spacing the same suffixes apart makes them the expressions they look like, which
    // is the whole rule stated the other way round.
    // Each yields a *value*, and producing a value is success, so every `||` here
    // skips its right side — these lines are here for the absence of a
    // `command not found`, not for a branch.
    let spaced = run_with_input(
        "a = 5\nb = 3\nxs = [7]\n\
         $a - 1 || puts minus\n\
         $a .. $b || puts range\n\
         $xs[0 + 0] || puts index\n\
         puts end\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&spaced.stdout),
        "end\n",
        "{}",
        String::from_utf8_lossy(&spaced.stderr)
    );
    assert!(
        spaced.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&spaced.stderr)
    );

    // The bound is the command word, so a variable with *argument-free* suffixes still
    // defers — `$p:base || puts failed` is the same idiom as `$cmd || puts failed`.
    for source in ["cmd = nosuchcmd\n$cmd", "p = /x/nosuchcmd\n$p:base"] {
        let out = run_with_input(&format!("{source} || puts fallback\n"));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("command not found: nosuchcmd"),
            "{source}: {:?}",
            out.stderr
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "fallback\n",
            "{source}"
        );
    }

    // `&` follows the same bound: a command word backgrounds, a comparison is refused
    // like any other non-command statement.
    let backgrounded = run_with_input("a = 5\n$a == 5 &\nputs after\n");
    assert!(
        String::from_utf8_lossy(&backgrounded.stderr).contains("backgrounding an expression"),
        "{:?}",
        backgrounded.stderr
    );
    assert_eq!(String::from_utf8_lossy(&backgrounded.stdout), "after\n");
}

/// The other operands with no command spelling, claimed by the same parse rather than
/// by a clause apiece: a range with a start, a modifier chain on a numeral, and a
/// negation. Each reported "command not found" for text that has exactly one reading.
#[test]
fn an_operand_with_no_command_spelling_is_a_value() {
    let out = run_with_input("1..3\n-1\n1:repr\n- 3\nx = 2\n- $x\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(out.stderr.is_empty(), "{:?}", out.stderr);

    // They are values, not just silence: each one's status is its own.
    let statuses = run_with_input("1:repr == \"1\" || puts wrong\nr = 1..3\nputs $r:len\n");
    assert_eq!(String::from_utf8_lossy(&statuses.stdout), "2\n");
    assert!(statuses.stderr.is_empty(), "{:?}", statuses.stderr);
}

/// **Statement** position is untouched: a spaced `<` / `>` there is still a redirect,
/// numeral or not. Only the condition reading was wrong, so only it changes.
#[test]
fn a_numeral_in_a_statement_still_redirects() {
    let dir = fresh_dir("numeral_statement_redirect");
    std::fs::write(dir.join("run.mesh"), "42 > out.txt\nputs after\n").unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("command not found: 42"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dir.join("out.txt").exists(),
        "the redirect should still open"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    let _ = std::fs::remove_dir_all(&dir);
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

/// A variable naming a command runs it, **with arguments**. `$editor > log` already
/// worked, since a redirect after the variable pushed the line to the command parser,
/// but `$editor file` reported `expected a statement separator`: the variable was
/// claimed as a value, the expression parser stopped after it, and the argument was
/// left unconsumed. A value is only a value when it is the whole statement.
#[test]
fn a_variable_naming_a_command_takes_arguments() {
    let out = run_with_input(
        "e = \"echo\"\n\
         $e hi\n\
         $e hi there\n\
         $e \"quoted arg\"\n\
         xs = [a b]\n\
         $e ...$xs\n\
         $e one && $e two\n\
         $e piped | cat\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi\nhi there\nquoted arg\na b\none\ntwo\npiped\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success());

    // An argument that *looks* postfix is still an argument when it is spaced off, the
    // same way `puts $x :len` prints `:len` rather than a length. Only an **attached**
    // postfix belongs to the command word, so `$e :len` echoes `:len` instead of
    // silently evaluating the length of the word `echo`.
    let out = run_with_input("e = \"echo\"\n$e :len\n$e .x\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ":len\n.x\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And the attached spelling still applies the modifier to the value.
    let out = run_with_input("x = \"abcd\"\nputs $x:len\nputs $x :len\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4\nabcd :len\n");
}

/// A connector after a *bare* variable picks the command too — `$cmd || fallback` is
/// the shell idiom it looks like. Reading `$cmd` as a string instead skipped running
/// the command altogether: no output, no side effects, and the branch decided by the
/// string's truthiness rather than the exit status, so `cmd = "false"` took the `&&`
/// arm because the *word* "false" is a non-empty string.
///
/// Every assertion here needs a command whose running is observable, which is what an
/// earlier version of this test missed by putting an argument before the connector —
/// the argument alone already forced the command path.
#[test]
fn a_connector_after_a_variable_command_still_runs_the_command() {
    // `false` fails, so the `||` arm runs and the `&&` arm does not.
    let out = run_with_input("cmd = \"false\"\n$cmd || puts failed\n$cmd && puts wrong\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "failed\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `true` succeeds, so the arms swap.
    let out = run_with_input("cmd = \"true\"\n$cmd && puts ran\n$cmd || puts wrong\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");

    // The command's own output appears, which is what proves it ran at all: `echo`
    // with no arguments prints an empty line before the second statement's output.
    let out = run_with_input("cmd = \"echo\"\n$cmd && puts after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\nafter\n");

    // And `&` backgrounds the command rather than refusing to background a value.
    let out = run_with_input("cmd = \"true\"\n$cmd &\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("backgrounding an expression"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A postfix guard is *part* of the value statement rather than something
    // following it, so `$x if $b` stays the guarded value it always was. Getting this
    // wrong ran a command named by the value: `command not found: 5`.
    let out = run_with_input(
        "x = 5\n\
         $x if true\n\
         $x unless false\n\
         $x if false\n\
         puts done\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("command not found"),
        "a guarded value ran as a command: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A negation is the other kind of operand: it has no command reading, so `&&`
    // joins the value statement instead of making one. This is the case that
    // distinguishes the shape question from the statement question.
    let out = run_with_input(
        "b = false\n\
         not $b && puts negated\n\
         t = true\n\
         not $t || puts fellthrough\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "negated\nfellthrough\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The same for an operand carrying **postfix modifiers**, which is where the
/// one-token lookahead went wrong: in `$p:base arg` the token after the variable is
/// the `:` of the modifier, so nothing saw the argument that followed the operand.
/// A redirect after such an operand is a redirect too, in every spelling —
/// spaced `>`, attached `>out`, and `>>`, none of which reached the command before.
#[test]
fn a_modified_operand_names_a_command_and_takes_a_redirect() {
    let dir = fresh_dir("modified_operand_command");
    let program = dir.join("hello");
    std::fs::write(&program, "#!/bin/sh\necho \"hello ran: $*\"\n").unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // `:base` of "sub/hello" is "hello", so each of these runs that program.
    let run = |body: &str| {
        std::fs::write(dir.join("run.mesh"), format!("p = \"sub/hello\"\n{body}")).unwrap();
        let out = mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        let written = std::fs::read_to_string(dir.join("out.txt")).unwrap_or_default();
        let _ = std::fs::remove_file(dir.join("out.txt"));
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            written,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // An argument after the modified operand.
    let (stdout, _, stderr) = run("$p:base arg\n");
    assert_eq!(stdout, "hello ran: arg\n", "{stderr}");

    // A redirect after it, in all three spellings.
    for body in [
        "$p:base > out.txt\n",
        "$p:base >out.txt\n",
        "$p:base >> out.txt\n",
        "$p:base arg > out.txt\n",
    ] {
        let (_, written, stderr) = run(body);
        assert!(stderr.is_empty(), "{body} errored: {stderr}");
        assert!(
            written.starts_with("hello ran:"),
            "{body} did not redirect the command: {written:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The redirect lookahead parses only the operand a **command word** can be — a
/// primary with its accesses, calls, and modifiers — and stops there. Going wider
/// absorbs operators that belong to the value: parsing through arithmetic took the
/// `+ 1` in `$x + 1 > 1`, decided the `>` was a redirect, and turned a boolean value
/// statement into a command that also truncated a file named `1`.
///
/// In a scratch directory because that is the failure: it creates the file.
#[test]
fn a_comparison_with_arithmetic_on_the_left_is_not_a_redirect() {
    let dir = fresh_dir("arithmetic_before_comparison");
    let run = |body: &str| {
        std::fs::write(dir.join("run.mesh"), body).unwrap();
        let out = mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    for body in [
        "x = 1\n$x + 1 > 1\n",
        "x = 5\n$x - 1 > 1\n",
        "x = 5\n$x * 2 > 1\n",
        "x = 1\n$x + 1 < 1\n",
        // A *computed* index is a nested expression too, and a command word cannot
        // hold one — `$xs[0]` with a literal index is a single word and does redirect,
        // but `$xs[0 + 0]` is a word plus a subscript the expression parser owns.
        "xs = [7 8]\n$xs[0 + 0] > 0\n",
        "xs = [7 8]\n$xs[1 - 1] > 0\n",
    ] {
        let stderr = run(body);
        assert!(stderr.is_empty(), "{body} errored: {stderr}");
        for name in ["0", "1"] {
            assert!(
                !dir.join(name).exists(),
                "{body} redirected into a file named {name:?} after the operand"
            );
        }
    }

    // The literal-index spelling is a word, so it keeps the redirect reading it has on
    // `main` — the point being that the line between them is what a *word* can hold.
    std::fs::write(dir.join("run.mesh"), "xs = [7 8]\n$xs[0] > out.txt\n").unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("command not found: 7"),
        "a literal index should still name a command: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("out.txt").exists(), "and should still redirect");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The readings this must not disturb: an operand that *is* the whole statement stays
/// a value, a spaced comparison in a condition stays a comparison even with a modifier
/// on the left, and a non-place assignment target stays a syntax error about places
/// rather than becoming a command nobody asked to run.
#[test]
fn a_value_that_is_the_whole_statement_is_still_a_value() {
    // Silent: these are values, not commands, and a value statement prints nothing.
    let out = run_with_input("xs = [a b]\n$xs\n$xs:len\nm = [k: v]\n$m.k\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("command not found"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Comparisons in a condition, including with a modifier on the left.
    let out = run_with_input(
        "xs = [a b]\n\
         if $xs:len > 1 { puts gt }\n\
         if $xs:len < 5 { puts lt }\n\
         if $xs:len == 2 { puts eq }\n\
         puts guard if $xs:len > 1\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "gt\nlt\neq\nguard\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A derived value is not a place, and saying so is a *syntax* error — not an
    // attempt to run a command named by the value.
    for source in ["xs = [1 2]\n$xs:dedup = 9\n", "$env.PATH[0] = x\n"] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(2), "{source}");
    }
}

/// A leading `not` negates a **value**, so `if not $b { … }` is a condition rather
/// than a command named `not`. `DESIGN.md` writes the idiom that way, and two of the
/// three positions already read it so: a postfix guard and an assignment's
/// right-hand side both parse an expression directly. Only the paths through
/// `value_start_in` — an `if` or `while` condition, and statement position — were
/// left out, which made `if not $b` report `command not found: not`.
#[test]
fn a_leading_not_negates_a_value_in_every_position() {
    let out = run_with_input(
        "b = false\n\
         if not $b { puts if-form }\n\
         while not $b { puts while-form\n\
         b = true }\n\
         t = true\n\
         if not $t { puts wrong } else { puts negates-true }\n\
         if not not $t { puts double } else { puts wrong }\n\
         f = false\n\
         puts guard-form if not $f\n\
         x = not $t\n\
         puts assigned $x\n\
         if not false { puts literal-false }\n\
         if not true { puts wrong } else { puts literal-true }\n\
         puts guard-literal if not false\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // The literals matter as much as the variables: `x = not false` already
        // worked, so an `if` that could not say it was the same inconsistency one
        // level down.
        "if-form\nwhile-form\nnegates-true\ndouble\nguard-form\nassigned false\n\
         literal-false\nliteral-true\nguard-literal\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `not` is a **reserved word**, so it never names a command however the line
/// continues. Keeping a command of that name reachable is what the old
/// "only when what follows is value-shaped" test was for, and paying for it meant
/// three lookahead questions — is the operand value-shaped, does a redirect follow
/// the *completed* operand, is the negation the whole statement — one of them a
/// trial parse. `env` and `sh` are already reserved, and `func` and `return` already
/// cannot be function names; this word joins them.
///
/// The escape hatches are the ones any reserved word has: a path (`./not`) and a
/// quoted word (`"not" arg`).
#[test]
fn not_is_reserved_and_never_names_a_command() {
    let dir = fresh_dir("leading_not_command");
    let program = dir.join("not");
    std::fs::write(&program, "#!/bin/sh\necho \"real-not ran with: $*\"\n").unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run = |body: &str| {
        std::fs::write(dir.join("run.mesh"), body).unwrap();
        let out = mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // A `not` on `PATH` is on `PATH` for every one of these, so anything other than
    // "real-not ran with: …" is the word being read as the operator it now is.
    for body in [
        "not foo\n",
        "not --flag\n",
        "not true\n",
        "if not foo { puts took-branch }\n",
        "not true foo\n",
        "not true | cat\n",
        "x = 1\nnot $x foo\n",
    ] {
        let (stdout, _) = run(body);
        assert!(
            !stdout.contains("real-not ran with"),
            "{body} still reached a command named `not`: {stdout}"
        );
    }

    // A path and a quoted word are the escape hatches, and they still reach it.
    assert_eq!(run("./not foo\n").0, "real-not ran with: foo\n");
    assert_eq!(run("\"not\" foo\n").0, "real-not ran with: foo\n");

    // A function of that name is refused rather than defined-but-unreachable, the
    // way `func func` and `func return` already are.
    let (stdout, stderr) = run("func not(x) { puts fn-not $x }\n");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("reserved name and cannot be a function name"),
        "{stderr}"
    );

    // Nor does it name a binding — `func = 5` and `return = 6` are already syntax
    // errors, and a variable spelled `not` could never be read back in command
    // position anyway.
    let (stdout, stderr) = run("not = 5\nputs after\n");
    assert_eq!(stdout, "");
    assert!(stderr.contains("syntax error"), "{stderr}");

    // `not` as *data* is untouched — it is only the command-word position that is
    // reserved — and so is a word that merely starts with those three letters.
    assert_eq!(run("puts not\nx = \"not\"\nputs $x\n").0, "not\nnot\n");
    assert_eq!(run("notes = [a b]\nputs $notes:len\n").0, "2\n");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A *spaced* `<` / `>` is a comparison rather than a redirect, and that holds after
/// `not` too, so `if not $y > 2 { … }` negates the comparison rather than negating
/// `$y` and redirecting into a file named `2`.
///
/// In a scratch directory because that is the failure mode: if the reading ever goes
/// the other way, mesh writes files named `2` and `5` in the working directory, and a
/// test should not litter the source tree to fail.
#[test]
fn a_spaced_comparison_after_not_is_still_a_comparison() {
    let dir = fresh_dir("leading_not_comparison");
    std::fs::write(
        dir.join("run.mesh"),
        "y = 1\n\
         if not $y > 2 { puts negated-cmp } else { puts wrong }\n\
         xs = [a b]\n\
         if not $xs:len > 5 { puts negated-mod-cmp } else { puts wrong }\n",
    )
    .unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "negated-cmp\nnegated-mod-cmp\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Nothing was redirected into a file named after the comparison's operand.
    assert!(!dir.join("2").exists() && !dir.join("5").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A run of `not`s is stepped over in a loop and folded to its **parity**, rather
/// than recursing once per word and stacking one AST node each. A word of `not`
/// otherwise costs a parse frame, an eval frame, and a `Drop` frame, so thousands of
/// them — generated or pasted — aborted the shell by signal before it had an answer.
/// Reserving the word is what made this reachable: the chain used to be walked by a
/// lookahead that concluded such a line was a *command* and never built it.
#[test]
fn a_long_not_chain_does_not_overflow_the_stack() {
    for tail in ["foo", "true", "$b"] {
        let body = format!("b = false\n{}{tail}\n", "not ".repeat(20_000));
        let out = run_with_input(&body);

        // `.code()` is `None` when a process is killed by a signal — SIGABRT is how a
        // stack overflow surfaces — so this is the check that matters.
        assert!(
            out.status.code().is_some(),
            "`not`×20000 {tail} was killed by a signal: {:?}",
            out.status
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("overflow"),
            "stack overflow reported for {tail}: {stderr}"
        );
    }
}

/// Folding a run of `not`s to its parity is not an optimization the reader can see:
/// `not` yields a bool from its operand's truthiness, so every one past the second
/// only flips a bool that is already there. An odd run is `not $x`, an even one the
/// `not not $x` that coerces without inverting — for a bool operand and for a
/// non-bool alike.
#[test]
fn a_run_of_nots_keeps_its_parity() {
    let out = run_with_input(
        "x = true\n\
         a = not $x\n\
         b = not not $x\n\
         c = not not not $x\n\
         d = not not not not $x\n\
         puts $a $b $c $d\n\
         y = 1 == 1\n\
         e = not $y\n\
         f = not not $y\n\
         g = not not not $y\n\
         puts $e $f $g\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "false true false true\nfalse true false\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_duplication_sends_one_descriptor_where_another_points() {
    let dir = fresh_dir("dup_basic");
    let both = dir.join("both.txt");

    // `> file 2>&1`: stdout moves to the file, then stderr copies where stdout
    // now points — so both land there.
    let out = run_with_input(&format!(
        "sh -c 'echo O; echo E >&2' > {} 2>&1\n",
        both.display()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<String> = std::fs::read_to_string(&both)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(lines, ["E", "O"]);

    // `&> file` is defined as exactly that pair.
    let amp = dir.join("amp.txt");
    run_with_input(&format!(
        "sh -c 'echo O; echo E >&2' &> {}\n",
        amp.display()
    ));
    let text = std::fs::read_to_string(&amp).unwrap();
    assert!(text.contains('O') && text.contains('E'), "{text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplication_order_matters_as_it_does_in_every_shell() {
    // `2>&1 > file` is the classic gotcha: stderr copies stdout's *original*
    // destination (the terminal) and only then does stdout move to the file. So
    // the file gets stdout alone, and stderr comes out on the shell's stdout.
    let dir = fresh_dir("dup_order");
    let file = dir.join("out.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo O; echo E >&2' 2>&1 > {}\n",
        file.display()
    ));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "O\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "E\n",
        "stderr should have followed stdout's original destination"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_duplication_can_put_stderr_into_a_pipe() {
    // Uppercasing downstream proves the text really traversed the pipe rather
    // than reaching the terminal directly.
    let out = run_with_input("sh -c 'echo out; echo err >&2' 2>&1 | tr a-z A-Z\n");
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(
        lines,
        ["ERR", "OUT"],
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Without it, stderr stays out of the pipe.
    let out = run_with_input("sh -c 'echo out; echo err >&2' | tr a-z A-Z\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "OUT\n");

    // `|&` is the shorthand and still behaves the same.
    let out = run_with_input("sh -c 'echo out; echo err >&2' |& tr a-z A-Z\n");
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(lines, ["ERR", "OUT"]);
}

#[test]
fn ordering_holds_when_a_pipe_is_involved_too() {
    // `2>&1 > file |` inside a pipeline: stderr took the pipe (stdout's
    // destination at that point), then stdout moved to the file.
    let dir = fresh_dir("dup_pipe_order");
    let file = dir.join("out.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo out; echo err >&2' 2>&1 > {} | tr a-z A-Z\n",
        file.display()
    ));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ERR\n");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "out\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_greater_amp_takes_both_streams_to_a_file() {
    // `>&` is two operators under one spelling, told apart by the target: a
    // descriptor duplicates, anything else takes both streams to that file. It
    // is the csh/zsh spelling of `&>`, and reads more consistently than it —
    // every other redirect leads with its direction.
    let dir = fresh_dir("dup_greater_amp");
    let file = dir.join("both.txt");
    let out = run_with_input(&format!(
        "sh -c 'echo O; echo E >&2' >& {}\n",
        file.display()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(lines, ["E", "O"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_duplication_target_must_name_a_usable_descriptor() {
    // A descriptor-shaped target is a duplication, so an unusable number is an
    // error rather than a filename.
    let out = run_with_input("echo hi 2>&9\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("descriptor"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `&>` always means both streams, so a descriptor prefix on it is refused
    // rather than silently ignored.
    let out = run_with_input("echo hi 2&> f\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("&>"));
}

#[test]
fn a_duplication_uses_the_descriptor_its_direction_names() {
    // `<&` defaults to stdin, `>&` to stdout. Checked with stdout sent somewhere
    // observable: at a terminal both descriptors are the same device, so a test
    // that only looks at the terminal cannot tell the two apart.
    let dir = fresh_dir("dup_direction");
    let file = dir.join("out.txt");
    run_with_input(&format!("echo VISIBLE <&0 > {}\n", file.display()));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "VISIBLE\n");

    let out = run_with_input("echo PIPED <&0 | tr A-Z a-z\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "piped\n");

    // `>&2` moves stdout to stderr, so nothing is left on stdout.
    let out = run_with_input("echo OUT >&2\n");
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("OUT"));
}

#[test]
fn an_escaped_amp_is_still_a_literal_before_a_redirect() {
    let dir = fresh_dir("redir_escaped_amp");
    let esc = run_with_input(&format!("cd {}\necho hi\\&>f\ncat f\n", dir.display()));
    assert_eq!(String::from_utf8_lossy(&esc.stdout), "hi&\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_computed_target_on_greater_amp_is_refused_as_ambiguous() {
    // `>&` chooses between duplicating and naming a file by reading the token as
    // written, so a computed target has no honest answer: `>&$fd` reads as
    // "duplicate onto $fd" but would quietly create a file named `2`. Refusing
    // beats guessing, and both meanings have an unambiguous spelling.
    let dir = fresh_dir("dup_ambiguous");
    let out = run_with_input(&format!("cd {}\nfd = 2\necho HI >&$fd\n", dir.display()));
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1>&$fd"), "{stderr}");
    assert!(stderr.contains("&> $file"), "{stderr}");
    // Nothing was created by the reading it did not take.
    assert!(!dir.join("2").exists());

    // The two spellings it points at both work.
    let out = run_with_input("fd = 2\necho HI 1>&$fd\n");
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("HI"));

    let file = dir.join("both.log");
    run_with_input(&format!(
        "f = {}\nsh -c 'echo O; echo E >&2' &> $f\n",
        file.display()
    ));
    let mut lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(lines, ["E", "O"]);

    // `<&` is only ever duplication, so a computed target is unambiguous there
    // and stays allowed.
    let seen = dir.join("seen.txt");
    run_with_input(&format!("n = 0\necho VISIBLE <&$n > {}\n", seen.display()));
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "VISIBLE\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quoting_a_greater_amp_target_makes_it_a_filename() {
    // Quotes are the user saying "this is text", so `>& "2"` names a file rather
    // than duplicating onto descriptor 2.
    let dir = fresh_dir("dup_quoted_target");
    let out = run_with_input(&format!(
        "cd {}\nsh -c 'echo O; echo E >&2' >& \"2\"\n",
        dir.display()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(dir.join("2")).unwrap();
    assert!(text.contains('O') && text.contains('E'), "{text:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Heredocs (`<< END … END`)
// ---------------------------------------------------------------------------

#[test]
fn an_unquoted_heredoc_interpolates_its_body() {
    let out = run_with_input("name = world\ncat << END\nhello $name\nEND\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello world\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The same reference forms a double-quoted string takes, so the two cannot
    // disagree about what a reference means.
    let out = run_with_input("m = [k: v]\nxs = [a b]\ncat << END\n$m.k $xs[1] $xs:len\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "v b 2\n");
}

#[test]
fn a_quoted_delimiter_makes_the_body_raw() {
    // The bash convention, kept in DESIGN.md: quoting the delimiter turns off
    // interpolation *and* escapes, which is what makes a heredoc usable for
    // shell snippets and regexes.
    let out = run_with_input("name = world\ncat << 'END'\nhello $name and \\n\nEND\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello $name and \\n\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_unquoted_body_takes_the_double_quote_escapes() {
    let out = run_with_input("cat << END\na\\tb\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\tb\n");

    // An unknown escape stays literal rather than erroring: a body carries data
    // — regexes, Windows paths — where a stray backslash is ordinary text.
    let out = run_with_input("cat << END\n\\d+ C:\\path\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "\\d+ C:\\path\n");
}

#[test]
fn a_heredoc_is_buffered_until_its_delimiter_arrives() {
    // The reader takes one line at a time, so an open heredoc has to report
    // "incomplete" rather than failing on sight — every interactive and piped
    // use depends on it. A following command still runs.
    let out = run_with_input("cat << END\nbody\nEND\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "body\nafter\n");

    // An unterminated *quote* is still a hard error at end of input, since it
    // cannot be continued on the next line.
    let out = run_with_input("puts 'unterminated\nputs after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("syntax error"));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_heredoc_body_can_exceed_a_pipe_buffer() {
    // Bodies become a temporary file rather than a pipe, so one larger than the
    // 64 KiB pipe buffer cannot deadlock the shell against a command that has
    // not started reading yet.
    let mut source = String::from("wc -l << END\n");
    for i in 0..20_000 {
        source.push_str(&format!("line {i}\n"));
    }
    source.push_str("END\n");
    let out = run_with_input(&source);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "20000",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_heredoc_leaves_no_file_behind() {
    // The temporary is unlinked as soon as it is opened, so nothing survives the
    // command and nothing is reachable by name while it runs.
    //
    // The shell gets a private `TMPDIR`, which `std::env::temp_dir` honors, so
    // the directory holds this test's temporaries and no one else's. Counting
    // the shared system directory instead meant counting every other test's
    // heredocs too — three dozen of them, each visible for the instant between
    // its open and its unlink — and a neighbor caught inside that window
    // during the "before" snapshot, then gone by "after", failed this test for a
    // file it never created.
    let dir = fresh_dir("heredoc_temp");
    let mut child = mesh_command()
        .env("TMPDIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mesh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"cat << END\nbody\nEND\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "body\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let left: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the shell's temp dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("mesh-heredoc-"))
        .collect();
    assert!(
        left.is_empty(),
        "a heredoc temporary was left behind: {left:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_heredoc_delimiter_is_never_expanded() {
    // The parser has already used the delimiter to find the body, and only its
    // quoting reaches execution — so expanding it would turn a perfectly good
    // `<< $missing` into an unbound-variable error for a word whose expansion is
    // then discarded.
    let out = run_with_input("cat << $missing\nbody\n$missing\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "body\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_malformed_reference_in_a_heredoc_is_an_error() {
    // A heredoc promises a string's interpolation rules, so `${bad` cannot
    // quietly become literal text when `"${bad"` is a syntax error.
    let out = run_with_input("cat << END\n${bad\nEND\nputs after\n");
    // The *same* diagnostic a string gives, only labeled as coming from a
    // heredoc — the promise is that the two grammars agree, not merely that both
    // fail somehow.
    let string_form = run_with_input("puts \"${bad\"\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&string_form.stderr).replace("mesh: ", "mesh: heredoc: "),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // A quoted delimiter takes no interpolation at all, so the same text is data.
    let out = run_with_input("cat << 'END'\n${bad\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "${bad\n");
}

#[test]
fn a_malformed_unicode_escape_in_a_heredoc_is_an_error() {
    // `\u` *is* in the escape set, so a malformed one is an error rather than
    // literal text; only an escape the set does not contain at all stays as
    // written. Asserted against the string form so the two cannot drift.
    let out = run_with_input("cat << END\nbad \\u{zz}\nEND\nputs after\n");
    let string_form = run_with_input("puts \"bad \\u{zz}\"\nputs after\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&string_form.stderr).replace("mesh: ", "mesh: heredoc: "),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // An unrecognized escape is still ordinary text, and a valid `\u` still decodes.
    let out = run_with_input("cat << END\nC:\\path \\u{41}\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "C:\\path A\n");

    // A quoted delimiter takes no escapes at all.
    let out = run_with_input("cat << 'END'\nbad \\u{zz}\nEND\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bad \\u{zz}\n");
}

#[test]
fn a_heredoc_reference_ends_where_the_command_grammar_says_it_does() {
    // Chained access is one reference, not "first access then literal text":
    // stopping after `$o.inner` would resolve a map and fail. Each form is
    // asserted to produce exactly what the double-quoted string produces.
    for body in [
        "$o.inner.key",
        "$o.inner.key:upper",
        "$xs[0].key",
        "$xs[0].key:upper trailing",
        "$o.inner.key.",
    ] {
        let setup = "o = [inner: [key: deep]]\nxs = [[key: deep]]\n";
        let heredoc = run_with_input(&format!("{setup}cat << END\n{body}\nEND\n"));
        let string = run_with_input(&format!("{setup}puts \"{body}\"\n"));
        assert_eq!(
            String::from_utf8_lossy(&heredoc.stdout),
            String::from_utf8_lossy(&string.stdout),
            "{body}: {}",
            String::from_utf8_lossy(&heredoc.stderr)
        );
    }
}

#[test]
fn a_long_heredoc_body_is_read_in_linear_time() {
    // Read through a pipe the body arrives a line at a time, and completeness
    // used to be re-derived by re-parsing the whole buffer after each one —
    // quadratic in the body's length. The gate now waits for the delimiter line
    // directly, so doubling the body should roughly double the time, not
    // quadruple it. Compared as a ratio because absolute times vary by machine.
    let time = |lines: usize| {
        let body: String = (0..lines).map(|i| format!("line {i}\n")).collect();
        let start = std::time::Instant::now();
        let out = run_with_input(&format!("cat << END\n{body}END\nputs done\n"));
        assert!(
            String::from_utf8_lossy(&out.stdout).ends_with("done\n"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        start.elapsed()
    };
    // Warm the binary's page cache so the first measurement is not penalized.
    time(200);
    let small = time(2_000).as_secs_f64();
    let large = time(8_000).as_secs_f64();
    // 4x the lines: linear predicts ~4x, quadratic ~16x. A generous ceiling
    // keeps this from being flaky on a loaded machine while still failing the
    // quadratic reading, which measured well past 10x before the fix.
    assert!(
        large < small * 9.0 + 0.5,
        "8,000-line body took {large:.3}s against {small:.3}s for 2,000 — \
         that is quadratic, not linear"
    );
}

#[test]
fn concurrent_heredocs_get_distinct_temporary_files() {
    // Stages open their redirections concurrently, and empty bodies gave no
    // distinguishing information of their own, so the names have to be unique by
    // construction rather than by content.
    let mut source = String::new();
    let stages: Vec<String> = (0..40).map(|i| format!("cat << D{i}")).collect();
    source.push_str(&stages.join(" | "));
    source.push('\n');
    for i in 0..40 {
        source.push_str(&format!("D{i}\n"));
    }
    let out = run_with_input(&source);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("File exists"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// Here-strings
// ---------------------------------------------------------------------------

#[test]
fn a_here_string_feeds_its_word_as_input() {
    let out = run_with_input("cat <<< hello\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // bash's trailing newline, which is what makes this one line rather than a
    // partial one — worth pinning because `cat` alone would not show it.
    let out = run_with_input("wc -l <<< hi\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1");

    // The target is an ordinary word: it interpolates, and quoting suppresses
    // that exactly as it does in an argument.
    let out = run_with_input("n = world\ncat <<< \"hello $n\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world\n");
    let out = run_with_input("n = world\ncat <<< 'hello $n'\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello $n\n");

    // A quoted word stays one word — no splitting on the way in.
    let out = run_with_input("cat <<< \"a b c\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b c\n");
}

#[test]
fn a_here_string_works_wherever_a_redirection_does() {
    // A pipeline stage, alongside another redirection, and on a function.
    let out = run_with_input("tr a-z A-Z <<< hey | cat\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "HEY\n");

    let dir = fresh_dir("here_string_file");
    let path = dir.join("out.txt");
    let out = run_with_input(&format!(
        "tr a-z A-Z <<< mix > {}\n",
        path.to_string_lossy()
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "MIX\n");

    let out = run_with_input("func f() { cat }\nf <<< viafunc\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "viafunc\n");
}

#[test]
fn a_here_string_is_told_apart_from_a_heredoc_and_a_comparison() {
    // `<<<` must not be read as `<<` plus `<`: it takes no delimiter, so the
    // next line is an ordinary command rather than body text.
    let out = run_with_input("cat <<< onlyline\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "onlyline\nafter\n");

    // And `<<` is still a heredoc, buffering until its delimiter arrives.
    let out = run_with_input("cat << END\na\nb\nEND\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nb\nafter\n");

    // A spaced `<` in a condition is still a comparison, not a redirection.
    let out = run_with_input("i = 1\nif $i < 3 { puts less }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "less\n");
}

#[test]
fn a_here_string_target_must_be_one_word() {
    // The same rule every redirection target follows: a list would silently
    // pick one of several meanings, so it is refused and `cat` never runs.
    // (The shell's own status is `puts after`'s, so the evidence is that the
    // command produced no output, not the exit code.)
    let out = run_with_input("xs = [a b]\ncat <<< $xs\nputs after\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).is_empty(),
        "a list target should be refused"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // A spread is the spelling that says "several words", and a redirection
    // still takes exactly one, so it is refused too rather than joining them.
    let out = run_with_input("xs = [a b]\ncat <<< ...$xs\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ambiguous redirect"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn a_here_string_cannot_name_another_descriptor() {
    // `<<<` feeds stdin by definition, so a descriptor prefix has no meaning.
    // The message must read as a refusal, not as "expected a here-string".
    let out = run_with_input("cat 2<<< hi\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot name another descriptor"),
        "{stderr}"
    );
    assert!(!stderr.contains("expected a here-string"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn input_text_can_be_backgrounded() {
    // Refused while a background external's targets were opened by a helper
    // process reached through argv: arbitrary text cannot travel that way — a
    // body past the argument limit, an embedded NUL. The stage forks and
    // `execvp`s itself now, so the body reaches its own process as memory and
    // the temporary is written there.
    for spelling in ["cat <<< body &", "cat << END &\nbody\nEND"] {
        let out = run_with_input(&format!("{spelling}\nsleep 0.3\n"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "body\n",
            "{spelling}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Including a body no argument list could have carried.
    let body = "x".repeat(200_000);
    let out = run_with_input(&format!("sh -c 'wc -c' << END &\n{body}\nEND\nsleep 0.4\n"));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "200001",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_long_here_string_does_not_deadlock() {
    // Fed through the same unlinked temporary file a heredoc uses, so a body
    // past the pipe buffer cannot block the shell against a command that has
    // not started reading.
    let out = run_with_input("x = ''\nfor i in 1..=20000 { x += 'abcdefghij' }\nwc -c <<< $x\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "200001",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// $sh.status and $sh.pipestatus
// ---------------------------------------------------------------------------

/// Read both runtime entries in a *single* command, since reading one is itself
/// a command that would replace what the other reports.
fn status_line(source: &str) -> String {
    let out = run_with_input(&format!(
        "{source}\nputs \"$sh.status |\" ...$sh.pipestatus\n"
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn sh_status_is_the_last_commands_exit_status() {
    assert_eq!(status_line("true"), "0 | 0");
    assert_eq!(status_line("false"), "1 | 1");
    assert_eq!(status_line("sh -c 'exit 42'"), "42 | 42");

    // Before anything has run it is 0, not an error or an unbound read.
    let out = run_with_input("puts $sh.status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");

    // Readable everywhere a value is: interpolation, comparison, a guard.
    let out = run_with_input("false\nputs \"code $sh.status\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "code 1\n");
    let out = run_with_input("false\nif $sh.status == 1 { puts caught }\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "caught\n");
    let out = run_with_input("false\nputs guarded if $sh.status != 0\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "guarded\n");
}

#[test]
fn sh_pipestatus_breaks_a_pipeline_down_by_stage() {
    // A real list, not bash's magic array: indexable, measurable, filterable.
    assert_eq!(
        status_line("sh -c 'exit 3' | sh -c 'exit 0' | sh -c 'exit 7'"),
        "7 | 3 0 7"
    );

    // Capture it once and work from the copy: each read is itself a command, so
    // a second read would report *that* command's status instead. Same care
    // `$?` needs in a POSIX shell, and the reason a real list helps — one
    // capture keeps everything.
    let out = run_with_input(
        "sh -c 'exit 3' | sh -c 'exit 0' | sh -c 'exit 7'\n\
         p = $sh.pipestatus\nputs $p:len $p[0] $p[2]\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3 3 7\n");

    // Reading it twice really does see the intervening command, which is
    // behavior worth pinning rather than a wart to hide.
    let out = run_with_input(
        "sh -c 'exit 3' | sh -c 'exit 7'\n\
         n = $sh.pipestatus:len\nputs $n ...$sh.pipestatus\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2 0\n");

    let out = run_with_input(
        "sh -c 'exit 3' | sh -c 'exit 0' | sh -c 'exit 7'\n\
         bad = $sh.pipestatus:filter(func(c) { $c != 0 })\nputs ...$bad\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3 7\n");

    // A command that is not a pipeline still reports one entry.
    assert_eq!(status_line("sh -c 'exit 5'"), "5 | 5");
    assert_eq!(status_line("x = 1"), "0 | 0");
}

#[test]
fn a_command_condition_publishes_its_status_to_the_branch_it_picks() {
    // A condition is a command that ran, so the branch it chose can read what it
    // exited with — the detail bash's `$?` has in the same place. Without it a
    // `130` from Ctrl-C flattened to a generic failure, and every caller that
    // wanted the code had to run the command as a plain statement and read
    // `$sh.status` before branching.
    let failed = run_with_input(
        "if sh -c 'exit 3' { puts \"then=$sh.status\" } else { puts \"else=$sh.status\" }\n",
    );
    assert_eq!(String::from_utf8_lossy(&failed.stdout), "else=3\n");

    // The success arm reads the 0 that picked it, not whatever ran before it.
    let passed = run_with_input("sh -c 'exit 3'\nif sh -c 'exit 0' { puts \"then=$sh.status\" }\n");
    assert_eq!(
        String::from_utf8_lossy(&passed.stdout),
        "then=0\n",
        "{}",
        String::from_utf8_lossy(&passed.stderr)
    );

    // A `while` header is the same construct, so its body sees it too.
    let looped = run_with_input(
        "n = 0\nwhile $n < 2 { n = $n + 1\n  if sh -c 'exit 4' { puts no } else { puts $sh.status } }\n",
    );
    assert_eq!(String::from_utf8_lossy(&looped.stdout), "4\n4\n");

    // A **value** condition is not a command and has no status to report, so the
    // previous command's still stands — the rule a skipped guard already follows.
    let boolean = run_with_input("sh -c 'exit 3'\nif 1 == 1 { puts \"then=$sh.status\" }\n");
    assert_eq!(String::from_utf8_lossy(&boolean.stdout), "then=3\n");

    // The `if` itself still reports its *body's* status, not its condition's.
    assert_eq!(
        status_line("if sh -c 'exit 3' { puts x } else { true }"),
        "0 | 0"
    );

    // A condition its own trailing guard skipped never ran the command, so it
    // publishes nothing and the previous run stands — breakdown included, since
    // the two always describe the same run. Raised in review: this reported
    // `1 | 1` for what was `1 | 1 0`.
    let skipped = run_with_input(
        "false | true\nif puts no if false { puts T } else { puts \"$sh.status |\" ...$sh.pipestatus }\n",
    );
    assert_eq!(String::from_utf8_lossy(&skipped.stdout), "1 | 1 0\n");
}

#[test]
fn a_while_reports_its_last_passs_breakdown_not_its_final_tests() {
    // `while` is the one construct whose condition runs *after* its final pass,
    // so the failing test leaves the newest record — and when its code happens to
    // match the body's, nothing downstream can tell the two apart. The loop
    // reports its body's status, so the body's breakdown has to go with it.
    // Raised in review: this read `1 | 1` where the pass was `1 | 0 1`.
    let out = run_with_input(
        "n = 0\nwhile test $n -lt 1 { n = 1\n  true | sh -c 'exit 1' }\nputs \"$sh.status |\" ...$sh.pipestatus\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1 | 0 1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The same mistake in the value channel: the failing test produces a *status*,
    // which displaced the pass's value, so a `while` in tail position answered the
    // loop's code instead of what its last pass evaluated. Raised in review.
    let valued = run_with_input(
        "func f() { n = 0\n  while test $n -lt 1 { n = 1\n    7 + 0 } }\nputs f()\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&valued.stdout),
        "7\n",
        "{}",
        String::from_utf8_lossy(&valued.stderr)
    );
    // The reference: a value condition, which never had the problem, agrees.
    let reference =
        run_with_input("func f() { n = 0\n  while $n < 1 { n = 1\n    7 + 0 } }\nputs f()\n");
    assert_eq!(String::from_utf8_lossy(&reference.stdout), "7\n");

    // A loop whose condition was false from the start ran no pass, so it produced
    // nothing and reports its own 0 rather than restoring what it never made.
    let never = run_with_input("func g() { while false { 7 } }\nputs \"[$(g())]\"\n");
    assert_eq!(String::from_utf8_lossy(&never.stdout), "[]\n");
    assert_eq!(
        status_line("while false { sh -c 'exit 4' | true }"),
        "0 | 0"
    );
}

#[test]
fn a_pipeline_condition_keeps_its_breakdown_in_the_branch() {
    // The condition publishes one entry only when nothing nested already
    // recorded, so a pipeline used as a condition arrives with both stages
    // intact rather than flattened to its pipefail status.
    let out = run_with_input(
        "if sh -c 'exit 3' | sh -c 'exit 0' { puts no } else { puts \"$sh.status |\" ...$sh.pipestatus }\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3 | 3 0\n");
}

#[test]
fn sh_pipestatus_shows_a_sigpipe_that_pipefail_forgives() {
    // The pipefail rule ignores an upstream SIGPIPE, so `$sh.status` is 0 — but
    // the stage really did die of signal 13, and saying so is the whole point of
    // keeping a per-stage list rather than repeating the overall status.
    assert_eq!(status_line("yes | head -1 > /dev/null"), "0 | 141 0");
}

#[test]
fn sh_status_and_pipestatus_always_describe_the_same_run() {
    // The invariant worth pinning: a compound's status *is* its body's, so the
    // breakdown must stay the body's too rather than flattening to one entry.
    // This is a deliberate difference from bash, where a function call or an
    // `if` resets PIPESTATUS to its own single status. It holds here because
    // pipefail is always on, so a compound's status is exactly the pipefail
    // status of the pipeline the list describes.
    for source in [
        "if true { sh -c 'exit 4' | true }",
        "for i in [1 2] { sh -c 'exit 4' | true }",
        "func g() { sh -c 'exit 4' | true }\ng",
        "func g() { if true { sh -c 'exit 4' | true } }\ng",
        "match 1 { 1 => { sh -c 'exit 4' | true } }",
        "true && sh -c 'exit 4' | true",
        // A `while` with a *command* condition: the test that ends the loop runs
        // after the last pass, so its record is the newest one when the loop
        // reports the body's status.
        "n = 0\nwhile test $n -lt 1 { n = 1\n  sh -c 'exit 4' | true }",
    ] {
        assert_eq!(status_line(source), "4 | 4 0", "for: {source}");
    }

    // A compound whose body never ran produces its own status, one entry.
    assert_eq!(status_line("if false { sh -c 'exit 4' | true }"), "0 | 0");

    // And a compound that runs a pipeline but then reports something *else* is
    // no longer described by that pipeline, so the breakdown must not survive:
    // `return 7` ends the function at 7, not at the pipeline's 4.
    assert_eq!(
        status_line("func g() { sh -c 'exit 4' | true\n  fail 7 }\ng"),
        "7 | 7"
    );

    // And a later command replaces both together, never one of them.
    assert_eq!(status_line("sh -c 'exit 4' | true\nz = 1"), "0 | 0");
}

#[test]
fn the_sh_namespace_lists_its_runtime_entries() {
    let out = run_with_input("puts ...$sh:keys\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "status pipestatus pid ppid uid version interactive width stdin stdout stderr \
         jobs origin source options name args\n"
    );

    // A mistyped key is still a loud error, and `status` is not a reserved
    // name — only `sh` is, so an ordinary variable may be called that.
    let out = run_with_input("puts $sh.nope\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no `nope` in this map"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_with_input("status = mine\nputs $status\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "mine\n");
}

/// Give a pty a known size, hand it to mesh as the named descriptors, and read
/// back what `$sh.width` said. Returns `None` when the pty could not be opened.
///
/// Non-interactive on purpose: `-c` is enough to ask the question, and it avoids
/// the whole line-editor handshake a prompt-driven harness needs.
fn width_seen_on_pty(columns: u16, redirect: &[i32]) -> Option<String> {
    let (mut master, mut slave) = (-1, -1);
    if open_pty_pair(&mut master, &mut slave) != 0 {
        return None;
    }
    let size = libc::winsize {
        ws_row: 24,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: a `winsize` is passed for the request that reads one, on a
    // descriptor `openpty` just returned.
    unsafe { libc::ioctl(slave, mesh_platform::TIOCSWINSZ, &raw const size) };

    // The answer comes back on a pipe rather than the pty, so a case that leaves
    // stdout redirected can still be read — the point of several of these.
    let mut child = mesh_command();
    child
        .args(["-c", "puts $sh.width"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for fd in redirect {
        // SAFETY: `dup` of a live descriptor; `Stdio` owns and closes the copy.
        let copy = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
        match *fd {
            0 => child.stdin(copy),
            1 => child.stdout(copy),
            _ => child.stderr(copy),
        };
    }
    // With stdout on the pty there is no pipe to read, so the pty is where the
    // answer lands; both cases are covered by reading whichever one mesh wrote to.
    let out = child.output().expect("run mesh on a pty");
    // SAFETY: descriptors this function opened and still owns.
    unsafe {
        libc::close(slave);
    }
    let piped = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !piped.is_empty() {
        unsafe { libc::close(master) };
        return Some(piped);
    }
    let mut buffer = [0_u8; 64];
    // SAFETY: a buffer this function owns, on a descriptor it opened.
    let count = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
    unsafe { libc::close(master) };
    let read = usize::try_from(count).unwrap_or(0);
    Some(String::from_utf8_lossy(&buffer[..read]).trim().to_string())
}

/// `$sh.width` is the terminal's column count, which a prompt needs to draw a
/// rule or right-align a segment. Rough edge 7 from the config port: without it
/// the config forked `tput cols` **per prompt**, measured at 2.6ms against 2.0ms
/// for the whole prompt composition path — the decoration costing more than what
/// it decorated.
#[test]
fn sh_width_reports_the_terminals_columns() {
    // No terminal anywhere, so no width: `0` rather than a made-up 80, which is
    // what lets `if $sh.width == 0` be the test for it.
    let out = run_with_input("puts $sh.width\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");

    let Some(width) = width_seen_on_pty(100, &[1]) else {
        eprintln!("SKIP: sh_width_reports_the_terminals_columns (no pty available)");
        return;
    };
    assert_eq!(width, "100");

    // Asked of stdout first, then stderr, then stdin. A redirected stdout answers
    // `ENOTTY` rather than the terminal behind it, so `mesh script.mesh | less`
    // reaches the real width through stderr.
    assert_eq!(
        width_seen_on_pty(55, &[2]).expect("pty"),
        "55",
        "stderr should answer when stdout is redirected"
    );
    assert_eq!(
        width_seen_on_pty(37, &[0]).expect("pty"),
        "37",
        "stdin should answer when neither of the others is a terminal"
    );
}

/// `$sh.options` is a map of booleans, every one of them on out of the box.
#[test]
fn the_settings_map_reads_as_booleans_that_start_on() {
    let out = run_with_input("puts ...$sh.options:keys\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "bold-input command-notify cwd-report osc-title shell-integration\n"
    );

    let out = run_with_input(
        "for name in $sh.options:keys { puts $name $sh.options[$name] }\n\
         puts direct $sh.options.bold-input\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "bold-input true\ncommand-notify true\ncwd-report true\nosc-title true\n\
         shell-integration true\ndirect true\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A setting is writable, and the read that follows sees the new value — which is
/// what makes `$sh.options` the first part of `$sh` that is not a snapshot of
/// state the shell alone decides.
#[test]
fn a_setting_can_be_turned_off_and_back_on() {
    let out = run_with_input(
        "$sh.options.bold-input = false\n\
         puts off $sh.options.bold-input\n\
         $sh.options.bold-input = true\n\
         puts on $sh.options.bold-input\n\
         $sh.options[\"cwd-report\"] = false\n\
         puts subscript $sh.options.cwd-report\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "off false\non true\nsubscript false\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // One setting at a time: turning one off leaves the others where they were.
    let out = run_with_input("$sh.options.bold-input = false\nputs ...$sh.options:values\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "false true true true true\n"
    );
}

/// Every way of getting `$sh.options` wrong, and what each one says. A settings
/// map that accepts a typo is a setting silently not applied, so all of these are
/// refused rather than absorbed.
#[test]
fn the_settings_map_refuses_everything_but_a_boolean_by_name() {
    for (source, message) in [
        // A typo is not a new setting, and the message lists what there is.
        (
            "$sh.options.bold-imput = false\n",
            "$sh.options: no `bold-imput` in this map; the settings are bold-input",
        ),
        // Not coerced: `\"false\"` is a string, and a truthiness rule here would
        // turn the setting *on*.
        (
            "$sh.options.bold-input = \"false\"\n",
            "a setting is `true` or `false`, got a string",
        ),
        ("$sh.options.bold-input = 0\n", "got an integer"),
        // Wholesale, which would have to answer what an omitted key means.
        (
            "$sh.options = [bold-input: false]\n",
            "assign one setting at a time",
        ),
        // `+=` has no meaning for a boolean.
        (
            "$sh.options.bold-input += true\n",
            "a setting is set with `=`, not `+=`",
        ),
        // Nothing to reach into.
        (
            "$sh.options.bold-input.x = true\n",
            "a setting is a boolean, with nothing inside it",
        ),
        // `$sh` is the session's, so there is no other scope for `global` to name.
        (
            "func f() { global $sh.options.bold-input = false }\nf\n",
            "`global` cannot apply to `$sh`",
        ),
        // And the read-only entries stay read-only, by name.
        (
            "$sh.status = 3\n",
            "`$sh.status` is read-only; only `$sh.options` may be assigned",
        ),
        ("$sh.nope = 3\n", "$sh: no `nope` in this map"),
    ] {
        let out = run_with_input(source);
        assert_eq!(out.status.code(), Some(1), "{source}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{source} gave {stderr}");
    }

    // Refused means unchanged, not "changed to whatever we could make of it".
    let out = run_with_input("$sh.options.bold-input = \"false\"\nputs $sh.options.bold-input\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");
}

/// The settings are the session's, not a scope's: a function that turns one off
/// has turned it off for the shell, with no `global` needed and no shadow copy to
/// leave behind.
#[test]
fn a_setting_changed_in_a_function_stays_changed() {
    let out = run_with_input(
        "func quiet() { $sh.options.bold-input = false }\n\
         quiet\n\
         puts after $sh.options.bold-input\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after false\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `source FILE` runs a file's code in **this** shell, so what it defines and
/// assigns outlives it — the whole point, and what makes an rc file possible.
#[test]
fn source_runs_a_file_in_the_current_shell() {
    let dir = fresh_dir("source_current");
    std::fs::write(
        dir.join("lib.mesh"),
        "func greet(who) { puts hello $who }\nshared = from-lib\n",
    )
    .unwrap();
    let main = dir.join("main.mesh");
    std::fs::write(&main, "source lib.mesh\ngreet world\nputs $shared\n").unwrap();

    let out = mesh_command()
        .arg(main.to_str().unwrap())
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run the sourcing script");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello world\nfrom-lib\n"
    );
    assert!(out.status.success(), "{:?}", out.status);
}

/// `$sh.origin` and `$sh.source` answer "type is being evaluated, and where does it
/// live" — the two questions `DESIGN.md` leaves as a TODO, kept **orthogonal to
/// interactivity**. `$sh.source` reports the *innermost* file, so it changes across
/// a `source` and changes back afterwards.
#[test]
fn origin_and_source_describe_the_input_being_evaluated() {
    let dir = fresh_dir("source_origin");
    std::fs::write(dir.join("inner.mesh"), "puts inner $sh.origin $sh.source\n").unwrap();
    std::fs::write(
        dir.join("outer.mesh"),
        "puts outer $sh.origin $sh.source\n\
         source inner.mesh\n\
         puts back $sh.origin $sh.source\n",
    )
    .unwrap();
    let main = dir.join("main.mesh");
    std::fs::write(
        &main,
        "puts main $sh.origin $sh.source\nsource outer.mesh\n",
    )
    .unwrap();

    let out = mesh_command()
        .arg("main.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run the nesting script");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // A script is `script`; each sourced file is `sourced` and names itself;
        // and the outer file gets its own answer back once the inner one returns.
        "main script main.mesh\n\
         outer sourced outer.mesh\n\
         inner sourced inner.mesh\n\
         back sourced outer.mesh\n"
    );

    // The origins that are not files report themselves with an empty `$sh.source`.
    let command = run_with_args(&["-c", "puts $sh.origin [$sh.source]"]);
    assert_eq!(String::from_utf8_lossy(&command.stdout), "command []\n");
    let piped = run_with_input("puts $sh.origin [$sh.source]\n");
    assert_eq!(String::from_utf8_lossy(&piped.stdout), "stdin []\n");
}

/// A startup file is a sourced file, and reports itself as one. That is what lets
/// an rc file locate a sibling — the `${BASH_SOURCE[0]}` use case `DESIGN.md` cites
/// — and `$sh.name` cannot do it, since it never changes.
#[test]
fn a_startup_file_reports_itself_as_sourced() {
    let home = fresh_dir("source_startup");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("env.mesh"),
        "puts env $sh.origin $sh.source\nfrom-env = yes\n",
    )
    .unwrap();
    let main = home.join("main.mesh");
    std::fs::write(&main, "puts main $sh.origin\nputs $from-env\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .arg(main.to_str().unwrap())
        .env("XDG_CONFIG_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("run with a startup file");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "env sourced {}\n",
            config.join("env.mesh").display()
        )),
        "{stdout}"
    );
    // The script's own frame is restored, and what the startup file set persists.
    assert!(stdout.contains("main script\n"), "{stdout}");
    assert!(stdout.contains("yes\n"), "{stdout}");
}

/// A startup file that leaves through `return` must **publish** its status, because
/// a startup file never passes through `run_recorded` — the funnel that normally
/// does it. Otherwise the next file in the chain reads `$sh.status` as whatever ran
/// before, while receiving the returned code as its `last`: two answers for one run.
/// `-i` makes a session interactive whatever its input is: `rc.mesh` is sourced
/// and `$sh.interactive` is true, so the half of a config behind
/// `return unless $sh.interactive` can be exercised without a pty. Rough edge 30
/// from the config port.
#[test]
fn dash_i_makes_a_session_interactive_whatever_its_input_is() {
    let home = fresh_dir("force_interactive");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("env.mesh"), "puts env=$sh.interactive\n").unwrap();
    std::fs::write(config.join("rc.mesh"), "puts rc-ran\n").unwrap();
    let script = home.join("main.mesh");
    std::fs::write(&script, "puts \"$sh.origin $sh.interactive\"\n").unwrap();

    let mesh = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mesh"))
            .args(args)
            .env("XDG_CONFIG_HOME", &home)
            .stdin(Stdio::null())
            .output()
            .expect("run mesh")
    };

    // Without it a script is not interactive, and `rc.mesh` is not in the set.
    let plain = mesh(&[script.to_str().unwrap()]);
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout),
        "env=false\nscript false\n",
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );

    // With it the same script sources `rc.mesh` and reports true — while its
    // origin stays `script`, since where the commands come from is a separate
    // question from what kind of session this is.
    let forced = mesh(&["-i", script.to_str().unwrap()]);
    assert_eq!(
        String::from_utf8_lossy(&forced.stdout),
        "env=true\nrc-ran\nscript true\n"
    );

    // `-c` too, and `--norc` still wins over the rc file it would have added.
    let commanded = mesh(&["-i", "-c", "puts $sh.interactive"]);
    assert_eq!(
        String::from_utf8_lossy(&commanded.stdout),
        "env=true\nrc-ran\ntrue\n"
    );
    let no_rc = mesh(&["-i", "--norc", "-c", "puts $sh.interactive"]);
    assert_eq!(String::from_utf8_lossy(&no_rc.stdout), "env=true\ntrue\n");

    // Piped stdin: an interactive session whose commands still came from a pipe,
    // so the origin stays `stdin`. `-i` says what kind of session this is; it does
    // not claim the commands were typed at a prompt, which is what `interactive`
    // as an origin means. Raised in review.
    let piped = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .arg("-i")
        .env("XDG_CONFIG_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(b"puts \"$sh.origin $sh.interactive\"\n")?;
            child.wait_with_output()
        })
        .expect("run mesh with piped stdin");
    assert_eq!(
        String::from_utf8_lossy(&piped.stdout),
        "env=true\nrc-ran\nstdin true\n"
    );

    // It does not conjure a terminal, so nothing a prompt would decorate with
    // leaks into a piped run's output — that stays byte-exact.
    let decorated = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .args(["-i", "-c", "puts hi"])
        .env("XDG_CONFIG_HOME", &home)
        .env("TERM", "xterm-256color")
        .env("TERM_PROGRAM", "vscode")
        .stdin(Stdio::null())
        .output()
        .expect("run mesh");
    assert_eq!(
        String::from_utf8_lossy(&decorated.stdout),
        "env=true\nrc-ran\nhi\n"
    );
}

/// `-i` says what kind of session this is; it does not claim the terminal. A
/// `fork` in a `-i` batch session must therefore stay in the invocation's process
/// group, because a group of its own is excluded from a `SIGINT` sent to that one
/// — which would kill the shell and leave the child running. Raised in review as
/// a P1.
#[test]
fn dash_i_does_not_claim_terminal_job_control() {
    let home = fresh_dir("force_interactive_pgid");
    std::fs::create_dir_all(home.join("mesh")).unwrap();

    // mesh is spawned without a session of its own, so it shares this process's
    // group; a `fork` child that is not given job control stays in it. Under an
    // interactive shell that took the terminal the child would `setpgid(0, 0)`
    // and become its own leader instead — which is the state that gets it missed
    // by a signal sent to the invocation's group.
    // SAFETY: `getpgrp` takes no arguments and cannot fail.
    let ours = unsafe { libc::getpgrp() };

    let out = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .args(["-i", "-c", "fork { sh -c 'ps -o pgid= -p $$' }"])
        .env("XDG_CONFIG_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("run mesh");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let child_pgid: i32 = stdout.trim().parse().expect("a process group id");
    assert_eq!(
        child_pgid, ours,
        "a `-i` batch session must leave a fork child in the invocation's group"
    );
}

#[test]
fn a_startup_file_that_returns_publishes_its_status() {
    let home = fresh_dir("source_startup_status");
    let config = home.join("mesh");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("env.mesh"),
        "puts env-runs\nfail 7\nputs never\n",
    )
    .unwrap();
    std::fs::write(config.join("login.mesh"), "puts login-sees $sh.status\n").unwrap();
    let main = home.join("main.mesh");
    std::fs::write(&main, "puts script-sees $sh.status\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mesh"))
        .arg("--login")
        .arg(main.to_str().unwrap())
        .env("XDG_CONFIG_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("run a login shell");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        // `login.mesh` sees the 7 `env.mesh` returned; the script then sees 0,
        // because `login.mesh`'s own `puts` is the last thing that ran.
        "env-runs\nlogin-sees 7\nscript-sees 0\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// How `source` fails. A **parse** error rejects the whole file, so a broken rc
/// cannot leave a half-defined config — `DESIGN.md` §"Error handling".
#[test]
fn source_reports_its_failures_with_the_shells_own_statuses() {
    let dir = fresh_dir("source_failures");
    std::fs::write(
        dir.join("bad.mesh"),
        "puts one\nthis is ( broken\nputs two\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("adir")).unwrap();

    let run = |body: &str| {
        let path = dir.join("run.mesh");
        std::fs::write(&path, body).unwrap();
        mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run")
    };

    // None of a file with a syntax error runs — neither statement around it.
    let bad = run("source bad.mesh\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&bad.stdout), "after\n");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("syntax error"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // The statuses `mesh FILE` itself uses, so a missing or unreadable file
    // answers the same however it is reached.
    for (body, code, message) in [
        ("source nope.mesh\n", 127, "No such file"),
        ("source adir\n", 126, "Is a directory"),
        ("source\n", 2, "needs a file to run"),
        ("source a b\n", 2, "takes exactly one file"),
    ] {
        let out = run(body);
        assert_eq!(out.status.code(), Some(code), "{body}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{body} gave {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `return` leaves the innermost unit that has an invoker to return **to**: a
/// function, or a sourced file — whose `source` takes the returned value's status.
/// A script, a `-c` string, and a typed line have no caller, so there it stays an
/// error. That is the distinction bash draws, and it is what makes an early-out in a
/// config file writable at all: `exit` would take the whole shell with it.
#[test]
fn return_leaves_a_sourced_file_and_gives_source_its_status() {
    let dir = fresh_dir("source_return");
    let run = |name: &str, body: &str, main: &str| {
        std::fs::write(dir.join(name), body).unwrap();
        std::fs::write(dir.join("run.mesh"), main).unwrap();
        mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run")
    };

    // A value becomes `source`'s status, and nothing after the `return` runs.
    let valued = run(
        "lib.mesh",
        "puts in-lib\nfail 3\nputs never\n",
        "source lib.mesh\nputs after $sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&valued.stdout), "in-lib\nafter 3\n");

    // A bare `return` carries the last status, exactly as a bare `exit` does.
    let bare = run(
        "lib.mesh",
        "sh -c \"exit 1\"\nreturn\nputs never\n",
        "source lib.mesh\nputs after $sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&bare.stdout), "after 1\n");

    // It stops the *innermost* file only: the outer one carries on with the
    // status it was handed.
    std::fs::write(dir.join("inner.mesh"), "puts inner\nfail 5\nputs never\n").unwrap();
    let nested = run(
        "outer.mesh",
        "puts outer\nsource inner.mesh\nputs outer-sees $sh.status\nputs outer-end\n",
        "source outer.mesh\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&nested.stdout),
        "outer\ninner\nouter-sees 5\nouter-end\n"
    );

    // A function is a nearer unit than the file holding it, so its `return` is the
    // function's and the file keeps going.
    let inside = run(
        "lib.mesh",
        "func f() { return 9 }\nx = f()\nputs fn $x\nfail 2\n",
        "source lib.mesh\nputs after $sh.status\n",
    );
    assert_eq!(String::from_utf8_lossy(&inside.stdout), "fn 9\nafter 2\n");

    // The guard a config file wants, now writable.
    let guard = run(
        "lib.mesh",
        "if $sh.interactive == false { puts batch\nreturn }\nputs interactive-only\n",
        "source lib.mesh\nputs done\n",
    );
    assert_eq!(String::from_utf8_lossy(&guard.stdout), "batch\ndone\n");
}

/// The other half: `exit` in a sourced file ends the **shell**, because `source`
/// runs in this shell rather than a child. And a top level with no caller still
/// refuses a `return`, naming both units that accept one.
#[test]
fn exit_in_a_sourced_file_ends_the_shell_while_a_script_refuses_return() {
    let dir = fresh_dir("source_exit");
    std::fs::write(dir.join("lib.mesh"), "puts in-lib\nexit 7\nputs never\n").unwrap();
    std::fs::write(dir.join("run.mesh"), "source lib.mesh\nputs never-either\n").unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "in-lib\n");
    assert_eq!(out.status.code(), Some(7));

    // A script's own top level has nothing to return to.
    std::fs::write(dir.join("plain.mesh"), "puts a\nreturn 3\nputs b\n").unwrap();
    let script = mesh_command()
        .arg("plain.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&script.stdout), "a\nb\n");
    assert!(
        String::from_utf8_lossy(&script.stderr)
            .contains("return: not inside a function or sourced file"),
        "{}",
        String::from_utf8_lossy(&script.stderr)
    );
}

/// `source` is a **status-producing command**, so whatever the file's last statement
/// produced stops at the `source`. Otherwise the file's value carries out and
/// `func f() { source lib.mesh }` returns whatever the file happened to end with —
/// a list, a string — where every other command yields its status.
#[test]
fn source_produces_a_status_not_the_files_last_value() {
    let dir = fresh_dir("source_status");
    let run = |tail: &str| {
        std::fs::write(dir.join("lib.mesh"), format!("x = \"hi\"\n{tail}\n")).unwrap();
        std::fs::write(
            dir.join("run.mesh"),
            "func f() { source lib.mesh }\ny = f()\nputs $y\n",
        )
        .unwrap();
        let out = mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // A string and a list both stop here; without the normalization the function
    // returned the value itself, and the list even needed a spread to print.
    assert_eq!(run("$x"), "0\n");
    assert_eq!(run("[1 2]"), "0\n");

    // An integer tail is a *value*, so it normalizes to a status of 0 like every
    // other value — and the same file run as a script exits 0 too, which is the
    // agreement that matters. `fail` is what a file uses to report a status.
    assert_eq!(run("42"), "0\n");
    std::fs::write(dir.join("lib.mesh"), "x = \"hi\"\n42\n").unwrap();
    let as_script = mesh_command()
        .arg("lib.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run as a script");
    assert_eq!(as_script.status.code(), Some(0));
}

// ---------------------------------------------------------------------------
// unset and global
// ---------------------------------------------------------------------------

#[test]
fn unset_removes_a_binding_rather_than_emptying_it() {
    // The two states that stand in for a missing null: `x = ""` is *bound to
    // empty*, `unset x` is *unbound*, and only the second makes a read fail.
    let out = run_with_input("x = ''\nputs empty if $x == ''\nunset x\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "empty\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("x: unbound variable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Several at once, and unsetting what was never bound anywhere is loud —
    // the same fail-loud rule a read follows — without stopping the shell.
    let out = run_with_input("a = 1\nb = 2\nunset a b\nunset nope\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope: unbound variable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // Blocks open no scope, so a binding made inside one is unset from outside.
    let out = run_with_input("if true { y = 1 }\nunset y\nputs gone if $sh.status == 0\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "gone\n");
}

#[test]
fn unset_acts_on_the_current_scope_only() {
    // Dropping a local reveals the global it was shadowing, because reads
    // resolve outward — `unset` removes a binding, it does not create a hole.
    let out =
        run_with_input("x = outer\nfunc f() { x = inner\n  unset x\n  puts $x }\nf\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "outer\nouter\n");

    // With no local to drop, plain `unset` leaves the global alone rather than
    // reaching through — the same rule that makes assignment local by default.
    let out = run_with_input("x = outer\nfunc f() { unset x }\nf\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "outer\n");

    // `global unset` is how you say you meant the global, symmetric with
    // `global name = value`.
    let out = run_with_input("x = outer\nfunc f() { global unset x }\nf\nputs $x\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("x: unbound variable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn global_writes_the_session_scope_from_inside_a_function() {
    let out = run_with_input(
        "count = 0\nfunc tick() { global count = $count + 1 }\ntick\ntick\nputs $count\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");

    // Without it, assignment is local by default and the global is untouched.
    let out = run_with_input("count = 0\nfunc tick() { count = 99 }\ntick\nputs $count\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");

    // `+=` too, and on a typed value rather than a string.
    let out = run_with_input("xs = [a]\nfunc add() { global xs += [b] }\nadd\nputs ...$xs\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b\n");

    // Appending to a name the *global* scope does not hold is an error, even if
    // a local of that name exists: `global` names one scope, not "whatever is
    // visible".
    let out = run_with_input("func f() { n = 1\n  global n += 1 }\nf\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("n: unbound variable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // A destructuring pattern puts every name in the one scope named.
    let out = run_with_input("func f() { global [p q] = [1 2] }\nf\nputs $p $q\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1 2\n");
}

#[test]
fn a_local_keeps_shadowing_a_global_it_just_wrote() {
    // `global x = …` writes the global; it does not retarget the local that is
    // already shadowing it, so the function still reads its own value. Surprising
    // enough to pin: the alternative (silently dropping the local) would make
    // `global` mean two different things depending on what came before.
    let out =
        run_with_input("x = out\nfunc f() { x = in\n  global x = set\n  puts $x }\nf\nputs $x\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "in\nset\n");

    // And scope stays lexical: a callee's `global` never touches its caller's
    // local, only the session scope.
    let out = run_with_input(
        "g = 0\nfunc inner() { global g = 9 }\n\
         func outer() { g = 1\n  inner\n  puts $g }\nouter\nputs $g\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n9\n");
}

#[test]
fn global_and_unset_are_only_keywords_where_a_statement_can_follow() {
    // Neither is reserved in `DESIGN.md` — only `env` and `sh` are — so both
    // must still work as ordinary variable names.
    let out = run_with_input("global = 5\nputs $global\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "5\n");
    let out = run_with_input("unset = 7\nputs $unset\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n");
    let out = run_with_input("global += 1\n");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("syntax error"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `unset` stays contextual *after* `global` too, not only at the start of a
    // statement: in `global unset = 9` the assignment operator says `unset` is
    // the name being bound. Consuming it as the operation would deny the global
    // scope a variable the local scope is allowed to have.
    let out = run_with_input("unset = 1\nglobal unset = 9\nputs $unset\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "9\n");
    let out = run_with_input("unset = 1\nglobal unset += 1\nputs $unset\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n");
    // …while `global unset NAME` still removes, since no operator follows.
    let out = run_with_input("x = outer\nfunc f() { global unset x }\nf\nputs after\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // The reserved namespaces cannot be unset.
    for name in ["env", "sh"] {
        let out = run_with_input(&format!("unset {name}\nputs after\n"));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("is reserved"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    }

    // `global` governs an assignment, so a command after it is refused with a
    // message that says which, rather than a bare "unexpected token".
    let out = run_with_input("func f() { puts hi }\nglobal f\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("it governs an assignment"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The message names both shapes now that `unset $m.key` is one of them.
    let out = run_with_input("unset\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("a name or place to unset"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

#[test]
fn export_writes_the_environment_children_inherit() {
    let out = run_with_input("export TESTV = child\nprintenv TESTV\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "child\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // It is the same write `$env.NAME =` performs, so the value reads back
    // through `$env` and `+=` appends.
    let out = run_with_input("export A = x\nexport A += y\nputs $env.A\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "xy\n");

    // Non-strings that have a byte form still cross.
    let out = run_with_input("export N = 42\nv = copied\nexport V = $v\nprintenv N\nprintenv V\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\ncopied\n");

    // Environment writes are global by design — a function's export persists,
    // the one deliberate exception to local-by-default.
    let out = run_with_input("func f() { export F = set }\nf\nputs $env.F\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "set\n");
}

#[test]
fn export_keeps_the_byte_boundary_rules() {
    // A path-type name is a list in the shell and `:`-joined on the way out,
    // the one exception to "only byte-strings cross".
    let out = run_with_input("export MANPATH = [/a /b]\nprintenv MANPATH\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/a:/b\n");
    let out = run_with_input("export MANPATH = [/a]\nexport MANPATH += /opt\nprintenv MANPATH\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/a:/opt\n");

    // An arbitrary list is still an error rather than a silent join, and an
    // embedded NUL cannot be represented at all.
    let out = run_with_input("export X = [a b]\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("only strings cross"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    let out = run_with_input("export X = \"a\\u{0}b\"\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("NUL"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

#[test]
fn bare_export_names_the_spelling_that_works() {
    // bash's `export NAME` marks an existing variable exported. mesh keeps shell
    // bindings and the environment in separate namespaces, so there is nothing
    // to mark — and the error has to say what to write instead rather than
    // leaving a reflex to fail obscurely.
    let out = run_with_input("x = 1\nexport x\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("export NAME = $NAME"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    let out = run_with_input("export\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("a NAME after `export`"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And like `global` / `unset`, it leads a statement only where one can
    // follow, so a variable may still be called `export`.
    let out = run_with_input("export = 5\nputs $export\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "5\n");
}

/// One name rule for reads and writes. The whole-string check behind `export NAME`,
/// `$env.KEY = …`, and a `${…}` place's root used to come from the compatibility
/// lexer, whose scan was ASCII-only, while `$name` reads went through the parser's
/// own — Unicode — scan. So a name could be bound and read but not exported.
#[test]
fn a_name_that_reads_can_also_be_exported() {
    let out = run_with_input(
        "café = 5\n\
         puts $café\n\
         _private = 6\n\
         puts $_private\n\
         export CAFÉ = x\n\
         puts $env.CAFÉ\n\
         export _PRIVATE = z\n\
         puts $env._PRIVATE\n\
         $env.naïve = y\n\
         puts $env.naïve\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "5\n6\nx\nz\ny\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Widening the head does not widen the rest of the shape: interior-only
    // hyphens still decide, so these stay refusals rather than becoming exports.
    for source in ["export 1x = v\n", "export a--b = v\n", "export x- = v\n"] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("syntax error"),
            "{source} was not refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// The rest of the read-only runtime $sh.* surface
// ---------------------------------------------------------------------------

#[test]
fn sh_reports_the_shells_own_process_ids() {
    // Checked against a child's view of its parent rather than against a
    // plausible-looking number, since any integer would look right.
    let out = run_with_input("puts $sh.pid\nsh -c 'echo $PPID'\n");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], lines[1], "$sh.pid is not the shell's own pid");

    // `$sh.ppid` is the test process, which is this program.
    let out = run_with_input("puts $sh.ppid\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        std::process::id().to_string()
    );

    // Both name the *shell*, not whichever process is reading them: bash's `$$`
    // and `$PPID` do not change inside a subshell, and a forked pipeline stage
    // is one. Reading them per access would report the stage's own ids here.
    let out = run_with_input("puts $sh.pid\nfunc f() { puts $sh.pid }\nf | cat\n");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], lines[1], "$sh.pid changed inside a forked stage");

    let out = run_with_input("puts $sh.ppid\nfunc f() { puts $sh.ppid }\nf | cat\n");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], lines[1], "$sh.ppid changed inside a forked stage");
}

#[test]
fn sh_reports_the_effective_user_id() {
    // Compare with the platform tool rather than assuming the test user. The
    // effective id is the one that decides the shell's process privileges.
    let out = run_with_input("puts $sh.uid\nid -u\n");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], lines[1], "$sh.uid is not the effective user id");

    // Like `$sh.pid`, this is session state and remains stable in a forked
    // in-shell pipeline stage.
    let out = run_with_input("puts $sh.uid\nfunc f() { puts $sh.uid }\nf | cat\n");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], lines[1], "$sh.uid changed inside a forked stage");
}

#[test]
fn sh_version_is_the_shells_own_version() {
    let out = run_with_input("puts $sh.version\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn sh_interactive_answers_which_loop_is_running() {
    // Piped input is not an interactive session…
    let out = run_with_input("puts $sh.interactive\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n");

    // …nor is `-c`, nor a script.
    let out = run_with_args(&["-c", "puts $sh.interactive"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n");
    let path = script("sh_interactive", "puts $sh.interactive\n");
    let out = run_with_args(&[path.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n");
}

#[test]
fn sh_stream_handles_are_descriptors_with_a_tty_test() {
    // A handle has no canonical byte form, so it never crosses to argv or into
    // a string — `DESIGN.md` puts it in the same row as a regex. The point of
    // the type is that this is a loud error rather than a printed `0`.
    for source in [
        "puts $sh.stdin\n",
        "puts \"fd=$sh.stdin\"\n",
        "export E = $sh.stdin\n",
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("stream handle"),
            "{source}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    }

    // Wherever a handle reaches a typed error, it is named as one. Grouping it
    // with the patterns compiled fine and reported "a glob" / "a pattern",
    // which is exactly the kind of wrong a new variant is supposed to prevent.
    for source in [
        "ys = [1]:map($sh.stdin)\n",
        "ys = [1]:filter($sh.stdin)\n",
        "xs = [$sh.stdin]\nputs ...$xs\n",
        "xs = [$sh.stdin]\ny = $xs:join(\",\")\n",
    ] {
        let stderr = String::from_utf8_lossy(&run_with_input(source).stderr).into_owned();
        assert!(stderr.contains("stream handle"), "{source}: {stderr}");
        assert!(
            !stderr.contains("glob") && !stderr.contains("pattern"),
            "{source}: {stderr}"
        );
    }

    // Every stream here is a pipe, so every answer is false.
    let out = run_with_input("puts $sh.stdin:tty $sh.stdout:tty $sh.stderr:tty\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false false false\n");

    // A test that can only ever see `false` would pass even if `:tty` always
    // returned it, so give the shell a real terminal on **one** stream: stdin is
    // a pty and stdout stays a pipe, an answer a constant could not produce.
    let (master, slave) = open_pty();
    let out = mesh_command()
        .args(["-c", "puts $sh.stdin:tty $sh.stdout:tty"])
        .stdin(unsafe { Stdio::from(std::os::fd::OwnedFd::from_raw_fd(slave)) })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run mesh");
    unsafe { libc::close(master) };
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "true false\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A scalar question maps element-wise over a list, as the file tests do.
    let out = run_with_input("fds = [$sh.stdin $sh.stdout]\nt = $fds:tty\nputs ...$t\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false false\n");

    // `:tty` asks about a handle, so a bare integer is a loud error rather than
    // a quiet answer about whatever descriptor happens to have that number.
    for source in ["x = abc\ny = $x:tty\n", "n = 1\ny = $n:tty\n"] {
        let out = run_with_input(&format!("{source}puts after\n"));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("requires a stream handle"),
            "{source}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    }
}

/// A (master, slave) pty pair. The slave is a genuine terminal, which is the
/// only way to make `isatty` answer true without a controlling tty in CI.
fn open_pty() -> (RawFd, RawFd) {
    let mut master = 0;
    let mut slave = 0;
    assert_eq!(open_pty_pair(&mut master, &mut slave), 0, "openpty failed");
    (master, slave)
}

// ---------------------------------------------------------------------------
// $sh.jobs
// ---------------------------------------------------------------------------

#[test]
fn sh_jobs_is_a_map_of_job_records() {
    // Empty until something is backgrounded, and a real map rather than text to
    // scrape — `:len` is the prompt-segment case `DESIGN.md` calls out.
    let out = run_with_input("puts $sh.jobs:len\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");

    let out = run_with_input("sleep 5 &\nputs $sh.jobs:len $sh.jobs[1].state $sh.jobs[1].cmd\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1 running sleep 5\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Keyed by job id in registration order, and indexable by that id — an
    // integer key reads the same as its string form, as for any map.
    let out = run_with_input("sleep 5 &\nsleep 6 &\nputs ...$sh.jobs:keys\nputs $sh.jobs[2].cmd\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1 2\nsleep 6\n");

    // `pid` is the group leader, which is what the launch notice reports and
    // what a signal would need.
    let out = run_with_input("sleep 5 &\nputs $sh.jobs[1].pid\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let announced = stderr
        .split_whitespace()
        .last()
        .expect("a `[1] <pgid>` launch notice");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), announced);

    // A missing id is a loud error, like any absent map key.
    let out = run_with_input("puts $sh.jobs[9].cmd\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no `9` in this map"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");

    // The records are values, so the filter from `DESIGN.md` works verbatim.
    let out = run_with_input(
        "sleep 5 &\nrunning = $sh.jobs:values:filter(func(j) { $j.state == running })\n\
         puts $running:len\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn sh_jobs_reports_a_finished_job_without_reaping_it() {
    // A finished process must not still read as `running` — the table only drops
    // a job when something asks, so the snapshot polls.
    let out =
        run_with_input("sh -c 'exit 7' &\nsleep 0.3\nputs $sh.jobs[1].state $sh.jobs[1].status\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "done 7\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `status` is empty while a job runs rather than standing in with a 0 that
    // would be indistinguishable from success.
    let out = run_with_input("sleep 5 &\nputs \"[$sh.jobs[1].status]\"\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[]\n");

    // Reading must not *remove* the job: reaping reports and drops it at its own
    // time, and a completed job stays available to `fg` until then. Observing
    // the table cannot be allowed to change what the shell does.
    let out = run_with_input("sleep 0 &\nsleep 0.3\nputs $sh.jobs[1].state\njobs\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("] Done (0) sleep 0"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\nafter\n");

    // A forked stage cannot poll — it is not the parent of the pids it
    // inherited — so it keeps the snapshot it was forked with rather than
    // reporting an empty table it has no grounds to claim.
    let out =
        run_with_input("sleep 5 &\nfunc f() { puts $sh.jobs:len }\nf | cat\nputs $sh.jobs:len\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exiting_over_a_finished_job_reports_nothing() {
    // The shell hangs up whatever is still in the job table on the way out. A
    // finished job is still in it: reading the table reaps the pid but keeps the
    // job, per `sh_jobs_reports_a_finished_job_without_reaping_it` above — so by
    // exit the group can have no members left and `kill` fails with `ESRCH`.
    // That is the hangup finding nothing left to do, not a fault to report;
    // bash exits silently over a finished job too.
    //
    // The command *after* the sleep is what makes this deterministic rather than
    // a coin flip: every executable refreshes `$sh.jobs`, so that refresh is the
    // poll that reaps `true`, and the sleep has already guaranteed it is dead.
    // Without it the only poll races `true`'s own exit, and the stray diagnostic
    // appears in roughly half of runs — which is how it reached `main`.
    let out = run_with_input("/bin/true &\nsleep 0.3\nputs after\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No such process"),
        "the exit hangup reported an already-finished job: {stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n", "{stderr}");

    // Silence is not the same as skipping the hangup: a job that really is
    // running must still receive it, so the suppression cannot widen into
    // leaving jobs behind. The job reports the signal through a file, since it
    // outlives the shell whose output the harness captures.
    let dir = fresh_dir("exit_hangup");
    let flag = dir.join("hupped");
    let out = run_with_input(&format!(
        "sh -c 'trap \"echo yes > {} ; exit\" HUP; sleep 5' &\nsleep 0.3\nputs after\n",
        flag.to_string_lossy()
    ));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !flag.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "a running job was never hung up on exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `fork { … }` is a subshell: the body runs in a forked child, so the process
/// state it changes is its own. `DESIGN.md` makes isolation explicit in three
/// grades and this is the strongest — the one that costs a process.
#[test]
fn a_fork_block_isolates_process_state() {
    let dir = fresh_dir("fork_isolation");

    // cwd: the child moves, the shell does not.
    std::fs::write(dir.join("run.mesh"), "fork { cd /tmp\npwd }\npwd\n").unwrap();
    let out = mesh_command()
        .arg("run.mesh")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let printed = String::from_utf8_lossy(&out.stdout);
    let mut lines = printed.lines();
    assert_eq!(lines.next(), Some("/tmp"), "{printed}");
    assert_ne!(
        lines.next(),
        Some("/tmp"),
        "the shell followed the child: {printed}"
    );

    // Bindings and the environment: written inside, unchanged outside.
    let out = run_with_input(
        "x = outer\n\
         fork { x = inner\n\
         puts inside $x }\n\
         puts after $x\n\
         $env.MESH_FORK_TEST = outer\n\
         fork { $env.MESH_FORK_TEST = inner }\n\
         puts env $env.MESH_FORK_TEST\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "inside inner\nafter outer\nenv outer\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The property that makes a subshell worth having: a nonzero `exit` inside one
/// ends the *child*, and arrives outside as an ordinary status. Its stdout still
/// crosses back, since bytes are the only thing that does.
#[test]
fn an_exit_inside_a_fork_block_does_not_end_the_shell() {
    let out = run_with_input("fork { puts before\nexit 3 }\nputs survived $sh.status\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "before\nsurvived 3\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the shell exited with the child");

    // The block's own status is the body's, so it composes with `&&` / `||`.
    let out = run_with_input(
        "fork { false }\nputs a $sh.status\n\
         fork { true } && puts and-ran\n\
         fork { false } || puts or-ran\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a 1\nand-ran\nor-ran\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `fork` is contextual, as `global` and `unset` are: it leads a statement only
/// where a subshell can follow, so a command of that name is still reachable.
/// Nothing in `DESIGN.md` reserves it, and reserving a word costs every program
/// that already has it.
#[test]
fn a_command_named_fork_is_still_reachable() {
    let dir = fresh_dir("fork_command");
    let program = dir.join("fork");
    std::fs::write(&program, "#!/bin/sh\necho \"real-fork: $*\"\n").unwrap();
    let mut permissions = std::fs::metadata(&program).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&program, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run = |body: &str| {
        std::fs::write(dir.join("run.mesh"), body).unwrap();
        let out = mesh_command()
            .arg("run.mesh")
            .current_dir(&dir)
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    assert_eq!(run("fork\n"), "real-fork: \n");
    assert_eq!(run("fork --flag\n"), "real-fork: --flag\n");
    assert_eq!(run("fork somewhere\n"), "real-fork: somewhere\n");
    // Only the brace makes it the keyword.
    assert_eq!(run("fork { puts subshell }\n"), "subshell\n");
    // Across a newline too, since `fork_expr` consumes them before the block and
    // `loop` / `if` both accept the same shape. It is the brace that decides, not
    // how much whitespace precedes it — and `fork\n` above still runs the command,
    // which is what keeps this from swallowing the bare invocation.
    assert_eq!(run("fork\n{ puts subshell }\n"), "subshell\n");
    assert_eq!(run("fork\n\n  { puts subshell }\n"), "subshell\n");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Backgrounding one is refused rather than quietly run in the foreground: a
/// backgrounded subshell needs a job-table entry of its own to be resumable, and
/// it does not have one yet.
#[test]
fn backgrounding_a_fork_block_is_refused_for_now() {
    let out = run_with_input("fork { puts hi } &\nputs after\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("backgrounding a `fork` block"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}

/// Control flow cannot cross the process boundary either, which falls out of the
/// isolation rather than being enforced: a `break` inside a subshell ends the
/// child, and the loop it appears to be inside — the parent's — keeps going. Same
/// for a `return` inside a function's `fork`.
#[test]
fn control_flow_does_not_escape_a_fork_block() {
    let out = run_with_input(
        "for i in [1 2 3] {\n\
         \x20 fork { if $i == 2 { break }\n\
         \x20   puts child $i }\n\
         \x20 puts parent $i\n\
         }\n\
         puts done\n",
    );
    // No `child 2`: the break ended that child. Every `parent` line is still
    // there, because the parent's loop never saw it.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "child 1\nparent 1\nparent 2\nchild 3\nparent 3\ndone\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_with_input(
        "func f() {\n\
         \x20 fork { return 5 }\n\
         \x20 puts after-fork-in-fn\n\
         }\n\
         f\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after-fork-in-fn\n");
}

/// A subshell inherits the shell's job table but is **not the parent** of the pids
/// in it. Its `waitpid` would fail with `ECHILD` and report every running job as
/// finished, so a child must not reap: it lists the snapshot it inherited, the
/// same rule a forked pipeline stage follows. `fg` and `bg` are refused there for
/// the stronger reason that they would hand the terminal to a job the parent
/// still believes it owns.
#[test]
fn a_fork_block_does_not_reap_or_resume_the_shells_jobs() {
    let out = run_with_input("sleep 5 &\nfork { jobs }\njobs\n");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        printed.matches("Running").count(),
        2,
        "the child reaped a job it does not own: {printed}"
    );
    assert!(
        !printed.contains("Done"),
        "a live job was reported finished: {printed}"
    );

    let out = run_with_input("sleep 5 &\nfork { fg }\nputs after\njobs\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no job control"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("after") && printed.contains("Running"),
        "the parent lost its job to the child: {printed}"
    );
}

/// `$(…)` drains the diverted pipe on a reader thread, so a `fork` inside one is the
/// shell forking while a thread of its own is running. A forked pipeline stage has
/// always done that, and the child still has to reach the interpreter and write back
/// through the captured descriptor rather than stall on the way.
#[test]
fn a_fork_block_inside_a_capture_still_captures() {
    let out = run_with_input(
        "before = $(pwd)\nout = $(fork { puts inside\ncd /\n })\n\
         after = $(pwd)\nmoved = $before != $after\n\
         puts \"captured [$out]\"\nputs \"moved $moved\"\n",
    );
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("captured [inside]") && printed.contains("moved false"),
        "a captured subshell must return its bytes and keep its `cd`: {printed}"
    );
}

/// A subshell is a fresh boundary, so its body starts at status 0 like every other
/// compound body — `false; fork { }` must not carry the failure from outside it
/// across the very edge the construct exists to draw.
#[test]
fn a_fork_body_starts_at_a_zero_status() {
    let out = run_with_input(
        "false\nfork { }\nputs empty $sh.status\n\
         false\nfork { true }\nputs ran $sh.status\n\
         false\nif true { }\nputs if-case $sh.status\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "empty 0\nran 0\nif-case 0\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A `fork` inside an already-forked stage must not touch job control: that
/// process is not the shell, so taking the terminal there would hand it to a
/// background job's group and leave the real shell without it once that job
/// exits. Exercised here for its *status* and output, since the terminal half
/// needs a tty; the guard itself is `in_forked_stage`.
#[test]
fn a_fork_nested_in_a_background_stage_still_runs() {
    let out = run_with_input(
        "func f() { fork { puts nested }\n  puts stage }\n\
         f &\n\
         sleep 0.3\n\
         puts shell-alive\n",
    );
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("nested") && printed.contains("stage") && printed.contains("shell-alive"),
        "{printed}"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `:repr` writes a value back as the source you would have typed for it.
///
/// The exactness is the point rather than the prettiness: the pairs below are
/// values that print identically under any ordinary display (`42` and `"42"`,
/// `[]` and `[:]`) and have to come out distinguishable here.
#[test]
fn repr_writes_a_value_as_the_literal_you_would_have_typed() {
    let out = run_with_input(
        "m = [k: 1, 'a b': [2, true]]\n\
         puts $m:repr\n\
         x = 42\n\
         s = \"42\"\n\
         puts $x:repr $s:repr\n\
         e = [:]\n\
         l = []\n\
         puts $e:repr $l:repr\n\
         puts $m:keys:repr\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "['k': 1, 'a b': [2, true]]\n42 '42'\n[:] []\n['k', 'a b']\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A value with no literal form is refused by name rather than approximated.
///
/// An approximation is worse than an error here: whatever it printed would read
/// back as a *different* value, which is exactly what `:repr` exists to rule out.
#[test]
fn repr_refuses_a_value_that_has_no_literal_form() {
    let out = run_with_input("puts $sh.stdin:repr\n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("a stream handle has no literal form"),
        "got {stderr}"
    );
}

/// Escaping is the inverse of the lexer's, so text that contains the quote, a
/// backslash, or a control character survives being written and read again.
#[test]
fn repr_quotes_text_that_would_otherwise_not_read_back() {
    let out = run_with_input(
        "a = \"it's\"\n\
         b = 'a$b'\n\
         c = 'tab\there'\n\
         puts $a:repr\n\
         puts $b:repr\n\
         puts $c:repr\n",
    );
    // `'…'` does not interpolate, so `$` needs no escape — that is why the
    // writer chooses it over `\"…\"`.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "'it\\'s'\n'a$b'\n'tab\\there'\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The smallest integer needs the sign folded into its literal, because its
/// magnitude has no positive counterpart. Typing it, reaching it by arithmetic,
/// and writing it back with `:repr` all have to agree.
#[test]
fn the_smallest_integer_is_a_literal_like_any_other() {
    let out = run_with_input(
        "typed = -9223372036854775808\n\
         computed = -9223372036854775807 - 1\n\
         same = $computed == $typed\n\
         puts $typed $computed\n\
         puts $typed:repr\n\
         puts $same\n\
         big = 9223372036854775807\n\
         puts $big\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "-9223372036854775808 -9223372036854775808\n\
         -9223372036854775808\n\
         true\n\
         9223372036854775807\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Which forms the sign folds into, and which keep the runtime negation.
///
/// One row per form so a failure names the form rather than a position in a
/// concatenated line. Every row here except the `i64::MIN` one was checked
/// against the pre-fold binary and is unchanged by it.
#[test]
fn folding_the_sign_claims_literals_and_nothing_else() {
    for (source, expected) in [
        // Folded: a bare magnitude that fits. The third is the whole reason the
        // fold exists — it has no positive counterpart to negate.
        ("x = -5", "-5"),
        ("x = -0", "0"),
        ("x = -9223372036854775808", "-9223372036854775808"),
        ("x = 9223372036854775807", "9223372036854775807"),
        // Not folded: the operand is not a bare literal, so the negation stays
        // and happens at runtime — same answer, different route.
        ("n = 5\nx = -$n", "-5"),
        // Double negation folds only the inner sign; the outer operand is then
        // signed rather than bare, so it negates at runtime as it always did.
        ("x = - -5", "5"),
        ("x = -(-5)", "5"),
        // Not a negation at all: `-abc` and `--5` each lex as one bare word, so
        // the fold never sees them, and a `-` between two operands is
        // subtraction.
        ("x = -abc", "-abc"),
        ("x = --5", "--5"),
        ("x = 3 - 5", "-2"),
    ] {
        let out = run_with_input(&format!("{source}\nputs $x\n"));
        assert!(
            out.status.success(),
            "{source:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{expected}\n"),
            "{source:?}"
        );
    }

    // Nothing to fold into, so these stay negations over operands that are not
    // integers — a loud error rather than a quiet string. A quoted `5` is a
    // string on purpose; past the range there is no literal in reach.
    for source in ["x = -\"5\"", "x = -99999999999999999999"] {
        let out = run_with_input(&format!("{source}\nputs $x\n"));
        assert!(!out.status.success(), "{source:?} should have failed");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("expected integer"),
            "{source:?} gave {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `:get(KEY, DEFAULT)` is the total accessor — the mesh spelling of bash's
/// `${VAR:-default}`, which every shell rc reaches for constantly.
#[test]
fn get_answers_a_default_where_a_strict_read_would_fail() {
    let out = run_with_input(
        r#"m = [editor: vim]
puts $m:get(editor, nano)
puts $m:get(pager, less)
xs = [a b c]
puts $xs:get(1, "-") $xs:get(9, "-") $xs:get(-1, "-")
"#,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "vim\nless\nb - c\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A bare `$env` is the whole table, which is what gives `$env:get(NAME, …)` an
/// ordinary map to work on. `$env.NAME` stays the strict read that errors.
#[test]
fn env_get_falls_back_where_a_strict_env_read_errors() {
    let out =
        run_with_input("puts $env:get(MESH_TEST_ABSENT, fallback)\nputs $env.MESH_TEST_ABSENT\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fallback\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("MESH_TEST_ABSENT"));
}

/// A bare `$env` is a map whose path-type entries are lists, so `puts $env` meets
/// the ordinary "a collection inside a collection has no rendering" rule. Pinned
/// alongside an ordinary nested map, because the point is that `$env` is **not** a
/// special case — making it printable would mean changing what `puts` does for
/// every map, which is a language decision rather than a fix here.
#[test]
fn a_bare_env_reads_through_its_accessors_rather_than_printing_whole() {
    let out = run_with_input("puts $env\n");
    let whole = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(whole.contains("has no rendering"), "{whole}");

    let out = run_with_input("m = [a: [1 2]]\nputs $m\n");
    let ordinary = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(ordinary.contains("has no rendering"), "{ordinary}");

    // The accessors are what it is for, and they work.
    let out = run_with_input(
        "puts $env:get(MESH_TEST_ABSENT, none)\nputs ($env:keys:len > 0)\nputs $env.PATH:len\n",
    );
    let mut lines = String::from_utf8_lossy(&out.stdout).into_owned();
    lines.truncate(lines.trim_end().len());
    let mut lines = lines.lines();
    assert_eq!(lines.next(), Some("none"));
    assert_eq!(lines.next(), Some("true"));
    // `PATH` is a list, which is the reason the whole map does not print.
    assert!(
        lines
            .next()
            .is_some_and(|count| count.parse::<u32>().is_ok()),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The affix family: drop a known prefix/suffix once, or peel a character set.
#[test]
fn the_affix_modifiers_strip_once_and_trim_repeatedly() {
    let out = run_with_input(
        r#"f = report.tar.gz
puts $f:stripend(".tar.gz")
puts $f:stripend(".zip")
puts $f:stripstart("report")
padded = "///a//"
puts $padded:trimstart("/") $padded:trimend("/")
spaced = "  hi  "
puts $spaced:trimstart:replaceall(" ", "_") $spaced:trimend:replaceall(" ", "_")
"#,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "report\nreport.tar.gz\n.tar.gz\na// ///a\nhi__ __hi\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The replace family. A **string** pattern matches verbatim and a **regex**
/// as a pattern — the same no-silent-coercion rule `~` follows — and the
/// anchored forms act only on a leading / trailing match.
#[test]
fn the_replace_modifiers_take_a_string_verbatim_and_a_regex_as_a_pattern() {
    let out = run_with_input(
        r#"s = "a.b.c"
puts $s:replaceall(".", "-")
puts $s:replaceall(/./, "-")
puts $s:replacestart("a", "Z") $s:replaceend("c", "Z")
puts $s:replacestart("b", "Z")
puts "one.js":replaceend(/\.js/, ".ts")
xs = [x.js y.js]
ys = $xs:replaceend(".js", ".ts")
puts ...$ys
"#,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a-b-c\n-----\nZ.b.c a.b.Z\na.b.c\none.ts\nx.ts y.ts\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The anchored replace spellings must ask the **engine** for a match at the
/// subject's edge, not filter an unanchored one's matches. `find_iter` reports
/// non-overlapping leftmost-first matches, so an earlier match eats the bytes a
/// later trailing one needed: `ab|bc` against `abc` reports only `ab`, and the
/// `bc` that really does end the string was never offered.
#[test]
fn an_anchored_regex_replace_finds_an_overlapped_match_at_the_edge() {
    let out = run_with_input(
        r#"puts "abc":replaceend(re("ab|bc"), "X")
puts "abc":replacestart(re("bc|ab"), "X")
puts "cab":replaceend(re("b|ab"), "X")
puts "abc":replacestart(/b/, "X")
puts "abc":replaceend(/b/, "X")
"#,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "aX\nXc\ncX\nabc\nabc\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The anchored spellings hand the edge requirement to the **engine** and search
/// the whole subject, so each edge reads the way regex reads. At the end the
/// engine tries start positions left to right and every candidate finishes at
/// `\z`, which makes it the longest trailing match; at the start every candidate
/// begins at 0, so leftmost cannot choose and regex's first-alternative rule
/// decides. The asymmetry is the engine's, not an accident of this code.
#[test]
fn an_anchored_replace_reads_each_edge_the_way_the_engine_does() {
    let out = run_with_input(
        r#"puts "abc":replaceend(re("c|bc"), "X")
puts "abc":replaceend(re("bc|c"), "X")
puts "abc":replacestart(re("a|ab"), "X")
puts "abc":replacestart(re("ab|a"), "X")
"#,
    );
    // The trailing edge is order-independent — both spellings find `bc`. The
    // leading edge follows the order written, as it does in any regex.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "aX\naX\nXbc\nXc\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A look-around assertion reads the bytes **around** the match, so the subject
/// has to stay whole. Testing truncated slices to find a longer match invented
/// context that was never there: `re(r"a\b")` has no match in `ab`, but against
/// the slice `a` the cut end looked like a word boundary and the assertion passed.
#[test]
fn an_anchored_replace_keeps_the_subject_whole_for_look_around() {
    let out = run_with_input(
        r#"puts "ab":replacestart(re(r"a\b"), "X")
puts "ab":replaceend(re(r"\bb"), "X")
puts "a b":replaceend(re(r"\bb"), "X")
"#,
    );
    // The first two have no match at all; only the third has a real boundary.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ab\nab\na X\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A multi-byte subject is matched and rebuilt on the match's own byte offsets,
/// so a character can never be split.
#[test]
fn an_anchored_replace_handles_a_multibyte_subject() {
    let out = run_with_input(
        "puts \"héllo\":replaceend(re(\"llo\"), \"X\")\nputs \"aé\":replaceend(re(\"é\"), \"X\")\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "héX\naX\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A regex literal needs delimiters that were **written** as delimiters. `\/a/`
/// and `/a\/` come out as the text `/a/` — beginning and ending with a slash
/// without either one being one — and outside a match slot that is the ordinary
/// string `/a/`. Reading them as patterns made identical text mean two different
/// things depending on where it sat.
///
/// This is the shared shape rule, so the fix reaches `~` and `match` arms as well
/// as the replace slot; all three are checked here. It predates the replace slot,
/// which inherited it by reusing `match_operand`.
#[test]
fn a_regex_literal_needs_delimiters_that_were_written_as_delimiters() {
    let out = run_with_input(
        r#"puts "x/a/y":replaceall(\/a/, X)
puts "x/a/y":replaceall(/a\/, X)
puts (match "xay" { \/a/ => REGEX; _ => LITERAL })
"#,
    );
    // The first two strip the literal three characters `/a/`, leaving `xXy`
    // rather than treating `a` as a pattern inside the slashes.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "xXy
xXy
LITERAL
"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `~` needs a pattern operand, so an escaped-delimiter word is now refused
    // by name instead of quietly matching as a regex.
    let out = run_with_input("if \"xay\" ~ \\/a/ { puts REGEX }\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("must be a regex or bare glob"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // An escape *inside* is untouched: this is a regex whose pattern contains an
    // escaped slash, and real delimiters still convert everywhere.
    let out =
        run_with_input("puts \"a/b\":replaceall(/a\\/b/, X)\nif \"xay\" ~ /a/ { puts REGEX }\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "X
REGEX
"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Quoting is what separates a **pattern** from the text it looks like: a bare
/// `/a/` is a regex here, while `"/a/"` is the three-character string. The
/// distinction is decided before the value reaches the modifier, which is why the
/// slot conversion belongs in the parser rather than at the point of use.
#[test]
fn quoting_separates_a_regex_literal_from_the_text_it_looks_like() {
    let out = run_with_input(
        r#"puts "abc":replaceall(/a/, X)
puts "x/a/y":replaceall("/a/", "/b/")
"#,
    );
    // The bare one matches `a` as a pattern; the quoted one matches the literal
    // three characters and leaves the rest of the path alone.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Xbc\nx/b/y\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The replace family's pattern slot converts a clean `/…/` literal and leaves
/// **everything else as written**. It is narrower than a `match` arm's slot,
/// which also reads a bare word as a glob — there is no glob reading here, so
/// converting one would make a word that should match itself fail instead.
#[test]
fn a_bare_word_in_a_replace_pattern_stays_the_string_it_looks_like() {
    let out = run_with_input(
        r#"puts "abc":replaceall(a, "X")
puts "abc":replacestart(a, "X")
puts "abc":replaceend(c, "X")
puts "Ab":replaceall(/a/:i, x)
puts "Ab":replacestart(/a/:i, x)
puts "aB":replaceend(/b/:i, x)
puts "/A/":replaceall(/a/:upper, X)
puts "/a/":replaceall(/a/:lower, X)
"#,
    );
    // A **flagged** literal converts: `/a/:i` is the regex wrapped in its flag
    // chain, so a check that only looked at the top of the converted tree
    // restored the word and left `:i` applied to a string.
    //
    // The last two do **not**. Traversal stops at anything that is not a regex
    // flag, because `/a/:upper` is the ordinary string expression `/A/`
    // everywhere else — reading it as a regex would both change its meaning and
    // fail, since `:upper` is not a flag.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Xbc\nXbc\nabX\nxb\nxb\nax\nX\nX\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The anchors are the **absolute** ones, not `^` / `$`, which move to line edges
/// under `:m` — and a subject's edge is not a line's. Regex flags still reach the
/// anchored form, since it is built from the same value.
#[test]
fn an_anchored_replace_uses_the_subject_edge_and_keeps_regex_flags() {
    let out = run_with_input(
        "puts \"a\\nb\":replaceend(re(\"b\"):m, \"X\")\nputs \"a.js\":replaceend(re(\"JS\"):i, \"ts\")\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\nX\na.ts\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Under `:x` a `#` comment runs to end of line, so the generated `)` and anchor
/// must not land on that line — they would become part of the comment and the
/// pattern would fail to compile even though the regex itself is fine.
#[test]
fn an_anchored_replace_survives_an_extended_mode_trailing_comment() {
    let out = run_with_input(
        r#"puts "abc":replaceend(re("bc # trailing comment"):x, "X")
puts "abc":replacestart(re("^a # leading"):x, "Z")
puts "abc":replaceend(re("(?x)bc # trailing comment"), "X")
puts "abc":replacestart(re("(?x)^a # leading"), "Z")
"#,
    );
    // The last two turn extended mode on from *inside* the pattern, where the
    // flags cannot see it — the reason the wrap is tried and retried rather than
    // chosen from `ignore_whitespace`.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "aX\nZbc\naX\nZbc\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The comment fallback must not mask a genuinely broken pattern: a swallowed
/// `)` always leaves the group unclosed, so a failure of both spellings reports
/// the original error rather than the retry's.
#[test]
fn an_anchored_replace_still_reports_a_genuinely_invalid_pattern() {
    let out = run_with_input("puts \"abc\":replaceend(re(\"(unclosed\"), \"X\")\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid regex"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
}

/// A modifier's string argument is flattened like its subject: which bytes are
/// split or stripped on must not depend on how they happen to be colored. Shared
/// with `:split` / `:join`, so this covers those too.
#[test]
fn a_modifier_takes_a_styled_string_argument_as_its_text() {
    let out = run_with_input(
        r#"suffix = style(".js", fg: red)
puts "a.js":stripend($suffix)
sep = style(":", fg: red)
puts "a:b":split($sep):len
xs = [x y]
puts $xs:join($sep)
"#,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n2\nx:y\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A styled key looks itself up: display attributes are rendering-only, so which
/// entry `:get` finds must not depend on how the key happens to be colored. The
/// subject and the replace family's pattern flatten the same way.
#[test]
fn get_looks_up_a_styled_key_as_its_text() {
    let out = run_with_input(
        r#"m = [k: ok]
key = style(k, fg: red)
puts $m:get($key, fallback)
puts $m:get(style(absent, fg: red), fallback)
"#,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\nfallback\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A replacement is literal: `regex`'s own `$1` expansion is suppressed, because
/// the capture-backreference spelling is still provisional in `DESIGN.md` and
/// taking `$1` now would freeze a syntax the design has not chosen.
#[test]
fn a_replacement_is_literal_text_not_a_backreference_template() {
    let out = run_with_input("puts \"ab\":replaceall(/a/, r\"$0-\")\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "$0-b\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An argument-taking modifier reads the same in **command position** as it does
/// on the right of an `=`. It used to be a syntax error there, since a command
/// word stops in front of the `(` and the arguments arrived glued to it.
#[test]
fn an_argument_taking_modifier_works_as_a_command_argument() {
    let out = run_with_input(
        r#"dirs = [/usr/bin /bin]
puts $dirs:join(":")
m = [k: v]
puts $m:get(k, none) $m:get(absent, none)
puts "a.b":stripend(".b")
"#,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/usr/bin:/bin\nv none\na\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The `(` has to **abut** the modifier name, exactly as it must abut a word for
/// an attached call. Spacing is the whole signal: `puts $x:upper (1)` is a chain
/// and a separate `(1)` argument, and reading it as `$x:upper(1)` would take an
/// argument the reader gave to `puts`.
#[test]
fn a_modifier_argument_list_must_abut_the_modifier_name() {
    let out = run_with_input("x = hi\nputs $x:upper (1)\nputs $x:upper ($x:len)\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "HI 1\nHI 2\n");
    // Every step of the run has to abut the one before it, not just the `(`: a gap
    // before the colon ends the chain too, leaving `:upper(lo)` the separate
    // modifier-reference call it reads as.
    let out = run_with_input("x = hi\nputs $x :upper(lo)\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi LO\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Attached, it really is a call — and an argument-free modifier says so.
    let out = run_with_input("x = hi\nputs $x:upper(1)\n");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not take arguments"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An **argument-free** modifier applies to a literal subject in command-argument
/// position, exactly as an argument-taking one already did. Requiring a trailing `(`
/// split one chain by whether its last step happened to take arguments, so
/// `puts abc:stripend("c")` was `ab` while `puts abc:upper` was the text `abc:upper`.
/// A `$`-prefixed subject never had the split — expansion applies its chain — so
/// only a literal one was affected.
#[test]
fn an_argument_free_modifier_applies_to_a_literal_in_command_position() {
    let out = run_with_input(
        "puts abc:upper\n\
         puts \"abc\":upper\n\
         puts abc:upper:lower\n\
         puts \"a.b\":stem\n\
         puts abc:stripend(\"c\")\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ABC\nABC\nabc\na\nab\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The spacing rule survives claiming the chain: a non-abutting `(` is still a
    // separate argument, for a literal subject as much as an expanded one.
    let out = run_with_input("puts abc:upper (1)\nx = hi\nputs $x:upper (1)\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ABC 1\nHI 1\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A *bare* dotted subject is a separate, preexisting limitation: the word ends at
    // the `.`, so the chain is never seen. Unchanged here — it does not resolve with
    // an argument list either, which is what shows this is not the split above.
    let out = run_with_input("puts a.b:stem\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.b:stem\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A map literal's `key: value` is not a modifier chain on the key. Both halves of an
/// attached chain have to abut, and without that check any map whose *value* word
/// happened to name a modifier was silently read as a chain: `[host: upper]` was the
/// string `HOST`, `[host: len]` was `4`, `[host: keys]` an error. Nothing reported it,
/// and the set of words that triggered it was the whole modifier vocabulary.
#[test]
fn a_map_literal_value_is_not_read_as_a_modifier_on_the_key() {
    // Assigned first: in command-argument position `[` opens a glob character class,
    // so a map literal only reaches the parser as a value.
    let out = run_with_input(
        "a = [host: upper]\n\
         b = [host: len]\n\
         c = [host: keys]\n\
         d = [host: build1, port: 22]\n\
         puts $a\nputs $b\nputs $c\nputs $d\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "host: upper\nhost: len\nhost: keys\nhost: build1\nport: 22\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Abutting on both sides is still the chain, so nothing about `$x:upper` moved.
    let out = run_with_input("x = abc\nputs $x:upper\nputs $x:split(\"b\"):len\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ABC\n2\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The reservation is of the **shape**, so only a bare identifier after the colon is
/// claimed. Everything else keeps the old punctuation reading, which is what leaves
/// `http://x`, `key:2` and `a:$b` alone.
#[test]
fn only_a_bare_identifier_after_the_colon_is_reserved() {
    let out = run_with_input(
        "b = z\n\
         puts key:2\n\
         puts key:/path\n\
         puts key:\n\
         puts http://x\n\
         puts a:$b\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "key:2\nkey:/path\nkey:\nhttp://x\na:z\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A modifier chain in `"…"` survives punctuation abutting it. The name was scanned
/// to a fixed list of delimiters (`. [ : ! /` and space), so anything absent from
/// that list was read *into* the name, matched no modifier, and reverted the whole
/// chain to literal text with no error — `"[$x:upper]"` rendered `[ab:upper]` while
/// `"$x:upper."` worked, purely because `.` happened to be listed.
#[test]
fn a_modifier_chain_survives_punctuation_after_it() {
    let out = run_with_input(
        "x = ab\n\
         puts \"[$x:upper]\"\n\
         puts \"($x:upper)\"\n\
         puts \"$x:upper,\"\n\
         puts \"$x:upper]\"\n\
         puts \"$x:upper}\"\n\
         puts \"$x:upper.\"\n\
         puts \"$x:upper:lower\"\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[AB]\n(AB)\nAB,\nAB]\nAB}\nAB.\nab\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A name that is not a modifier still ends the chain and stays text, and a name
    // run together with following letters is not a modifier at all.
    let out = run_with_input("x = ab\nputs \"$x:nosuch\"\nputs \"a$x:upperb\"\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ab:nosuch\naab:upperb\n"
    );
}

/// A **compact** map literal is a map, whatever its value word names. The key is
/// otherwise parsed by `expression`, whose postfix loop claims the colon first, so
/// `[host:upper]` built the string `HOST` and `[host:upper, port:22]` was a hard
/// "consistent map entries" error — silently, and only for the values that happened
/// to name a modifier.
#[test]
fn a_compact_map_literal_is_a_map_whatever_the_value_names() {
    let out = run_with_input(
        "a = [host:upper]\n\
         b = [host:upper, port:22]\n\
         c = [host:build1]\n\
         d = [a:1, b:2]\n\
         puts $a\nputs $b\nputs $c\nputs $d\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "host: upper\nhost: upper\nport: 22\nhost: build1\na: 1\nb: 2\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Only a **bare** word is claimed as a key, so every spelling that means a chain
    // inside a list still is one.
    let out = run_with_input(
        "x = abc\n\
         e = [\"abc\":upper]\n\
         f = [$x:upper]\n\
         g = [(host:upper)]\n\
         puts $e\nputs $f\nputs $g\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ABC\nABC\nHOST\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An attached `:modifier` outranks keyword parsing. A keyword is claimed only as a
/// *bare* word, so `if:upper` is a chain on the text `if`. The keyword arms return
/// before the postfix loop, so without the guard `if` / `match` / `for` were syntax
/// errors and `not:upper` was silently `false` — `not` took the negation and left
/// `:upper` to fold away, which is the worst of the four because nothing reports it.
#[test]
fn an_attached_modifier_outranks_keyword_parsing() {
    let out = run_with_input(
        "a = if:upper\n\
         b = not:upper\n\
         c = for:upper\n\
         d = match:upper\n\
         puts $a $b $c $d\n",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "IF NOT FOR MATCH\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Command position too, where `if` / `unless` also lead a trailing guard: the
    // chain wins, so `puts if:upper` is an argument rather than a guarded `puts`.
    let out = run_with_input("puts if:upper\nputs unless:upper\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "IF\nUNLESS\n");
    // The keywords and the guards are untouched — spacing is the signal, as it is
    // wherever else a chain is recognized.
    let out = run_with_input(
        "if true { puts yes }\n\
         x = if true { 1 } else { 2 }\n\
         puts $x\n\
         puts guarded if true\n\
         puts skipped if false\n\
         if :exists(\"Cargo.toml\") { puts found }\n",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "yes\n1\nguarded\nfound\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A bare `:name` after a value is still literal text when it names no modifier,
/// so recognizing an argument list cannot have changed `$host:$port`.
#[test]
fn a_non_modifier_colon_stays_literal_in_command_position() {
    let out = run_with_input("host = h\nport = 1\nputs $host:$port\nputs $host:upper\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "h:1\nH\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The modifiers say what they need rather than guessing, so a wrong argument
/// type or count is loud.
#[test]
fn a_misused_modifier_argument_is_a_loud_error() {
    for (source, message) in [
        ("xs = [a]\nputs $xs:get(0)\n", "takes exactly 2 arguments"),
        (
            "puts \"a\":replaceall(\"a\", 1)\n",
            "replacement must be a string",
        ),
        ("puts \"a\":stripend(\"\")\n", "must not be empty"),
        // The empty-pattern refusal must not depend on which spelling reached
        // for it: a regex arrives by a different route than a string, and an
        // empty one matches at every position.
        ("puts \"abc\":replaceall(//, X)\n", "must not be empty"),
        (
            "puts \"abc\":replaceall(re(\"\"), X)\n",
            "must not be empty",
        ),
        ("puts \"abc\":replacestart(//, X)\n", "must not be empty"),
        ("puts \"abc\":replaceend(//, X)\n", "must not be empty"),
        (
            "puts \"a\":replaceall([a], \"b\")\n",
            "pattern must be a string or a regex",
        ),
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "`{source}` did not report {message}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "`{source}` should have failed");
    }
}

/// `gets` must not read past its own newline: the rest of stdin belongs to
/// whatever runs next, so a buffered reader would silently eat it.
#[test]
fn gets_leaves_the_rest_of_stdin_for_the_next_command() {
    let mut command = mesh_command();
    command
        .arg("-c")
        .arg("gets first\nputs \"first=$first\"\ncat\n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn mesh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"one\ntwo\nthree\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "first=one\ntwo\nthree\n"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// End of input reports status 1 and leaves the variable alone, which is what
/// makes `while gets line { … }` terminate; a blank line does not.
#[test]
fn gets_reports_end_of_input_without_clobbering_its_variable() {
    let mut command = mesh_command();
    command
        .arg("-c")
        .arg("line = kept\nwhile gets line { puts \"[$line]\" }\ngets line\nputs \"last=$line status=$sh.status\"\n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn mesh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"a\n\nb")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[a]\n[]\n[b]\nlast=b status=1\n"
    );
}

/// `gets` refuses the namespace names, and does so **before** reading, so a
/// rejected operand does not also swallow the line the caller could retry with.
/// A binding under `env` or `sh` could never be read back — resolution always
/// takes those names as the namespaces.
#[test]
fn gets_refuses_a_reserved_namespace_without_consuming_input() {
    for name in ["env", "sh"] {
        let mut command = mesh_command();
        command
            .arg("-c")
            .arg(format!("gets {name}\ncat\n"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn mesh");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"untouched\n")
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait for mesh");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("reserved namespace"),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The line is still there for whatever runs next.
        assert_eq!(String::from_utf8_lossy(&out.stdout), "untouched\n");
    }
}

/// Invalid UTF-8 is **refused**, not replaced with U+FFFD. `gets` reads data in,
/// so it follows the capture — which rejects a non-UTF-8 stream — rather than
/// `$env`, whose lossy read renders a table the shell was handed. Binding
/// corrupted text and reporting success would outlive any chance of noticing.
#[test]
fn gets_refuses_a_non_utf8_line_and_leaves_its_variable_alone() {
    let mut command = mesh_command();
    command
        .arg("-c")
        .arg("v = kept\ngets v\nputs \"v=$v status=$sh.status\"\n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn mesh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"a\xffb\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for mesh");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not valid UTF-8"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Status 2, not the 1 that means end of input — a read error must not end a
    // `while gets line` as though the input had simply run out.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "v=kept status=2\n");
}

/// `gets` checks its operand, and takes at most one.
#[test]
fn gets_rejects_a_bad_operand() {
    for (source, message) in [
        ("gets 1bad\n", "is not a variable name"),
        ("gets a b\n", "takes at most one variable name"),
    ] {
        let out = run_with_input(source);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "`{source}` did not report {message}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
