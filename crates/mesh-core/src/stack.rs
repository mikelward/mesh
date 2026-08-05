//! What happens when the shell runs off the end of its stack.
//!
//! A recursive-descent parser and a tree-walking evaluator both recurse in
//! proportion to how nested their input is, so *some* input is always too deep.
//! [`MAX_DEPTH`](crate::parser) refuses the nesting the parser can see coming,
//! and that is the real fix — an error, at a sensible place, naming the problem.
//!
//! This module is for everything that limit cannot see. It does not cover the
//! evaluator walking a long operator spine (`1 + 1 + …` parses *iteratively*, so
//! it is never deep at parse time), it does not cover a stack far smaller than
//! the usual one (`ulimit -s 1024` moves the parser's own ceiling below the
//! limit), and it cannot cover a recursion nobody has thought of yet.
//!
//! Left alone, all of those arrive as a `SIGSEGV` on the guard page, which the
//! Rust runtime turns into an abort: `thread 'main' has overflowed its stack` and
//! a process killed by a signal. Nothing a script can test for, nothing a reader
//! can act on, and an interactive session that simply vanishes.
//!
//! [`install_fault_reporting`] makes that outcome legible. It is a last resort
//! and deliberately not a recovery — the shell still exits, because unwinding out
//! of a signal handler is not something Rust can do safely — but it exits saying
//! what happened, with a status a caller can test.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::exec;

/// The two ends of the stack the shell is running on, published for [`on_fault`]
/// to compare a faulting address against. Zero means "not known", under which a
/// fault reads as an ordinary bad address rather than an overflow — the cautious
/// way round, since claiming an overflow that was really a memory bug would send
/// a reader looking in the wrong place.
static STACK_LOW: AtomicUsize = AtomicUsize::new(0);
static STACK_HIGH: AtomicUsize = AtomicUsize::new(0);

/// Whether stderr was a terminal at startup, decided *before* any fault because
/// `isatty` is not among the calls a signal handler may make.
///
/// It is the shell's *own* stderr that is sampled, so a builtin running under a
/// redirect (`pwd extra 2>file`) can be writing somewhere else when the fault
/// lands, and the carriage returns below would go to a file. That is the whole
/// cost of getting it wrong — a stray `\r` in the one line printed by a shell
/// that is already dying — against a message that would otherwise walk diagonally
/// down the screen in the interactive case this exists for.
static STDERR_IS_TERMINAL: AtomicBool = AtomicBool::new(false);

/// How far below [`STACK_LOW`] still counts as this stack's guard region. The
/// fault from a deep recursion lands just past the end; a single page is the
/// smallest window that contains it, and a large frame can step further, so this
/// allows several.
const GUARD_SLACK: usize = 64 << 10;

/// The status a fatal fault exits with.
///
/// Distinct from the `2` a syntax error uses, because the two say different
/// things to a script: `2` is "your input was bad", this is "the shell could not
/// continue". 70 is `EX_SOFTWARE` from `sysexits.h`, the nearest thing to a
/// convention for an internal failure.
const EXIT_FAULT: u8 = 70;

/// Arrange for a stack overflow to be reported rather than aborted.
///
/// Every step is best-effort and independently useful: without the alternate
/// stack the handler cannot run, without the handler the fault aborts, and
/// without the bounds the message is vaguer. A failure in any of them leaves the
/// shell exactly as it was before this module existed, which is why none of them
/// is fatal — but each says so, rather than leaving a guarantee quietly missing.
pub fn install_fault_reporting() {
    use std::io::IsTerminal as _;
    STDERR_IS_TERMINAL.store(std::io::stderr().is_terminal(), Ordering::Relaxed);
    publish_stack_bounds();
    install_alternate_stack();
    install_fault_handler();
}

/// Record where this thread's stack lives.
///
/// The high end is the address of a local, which is within a frame or two of the
/// top. The size is the process's own `RLIMIT_STACK`, which is what the kernel
/// actually enforces on the initial thread — reading it rather than assuming
/// 8 MiB is what makes the message right under `ulimit -s`, which is exactly the
/// case where an overflow is most likely and a wrong guess least excusable.
fn publish_stack_bounds() {
    let anchor = 0_u8;
    let high = std::ptr::addr_of!(anchor) as usize;
    // SAFETY: `getrlimit` writes a complete `rlimit` on success, and the zeroed
    // value is never read unless it does.
    let size = unsafe {
        let mut limit: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_STACK, &mut limit) != 0 {
            return;
        }
        limit.rlim_cur
    };
    // `RLIM_INFINITY` is not a distance below `high`; without a size there is no
    // low end to compare against, so the handler falls back to its general
    // message rather than testing against a nonsense bound.
    if size == libc::RLIM_INFINITY {
        return;
    }
    STACK_HIGH.store(high, Ordering::Relaxed);
    STACK_LOW.store(high.saturating_sub(size as usize), Ordering::Relaxed);
}

/// Give this thread a separate stack for signal handlers to run on.
///
/// Without it a handler for a stack overflow is unrunnable by definition: the
/// fault happens because there is no room left, and delivering a signal needs
/// room. The Rust runtime installs one of these for its own overflow message;
/// this one is installed explicitly rather than inherited, so the guarantee does
/// not rest on that continuing to be true.
fn install_alternate_stack() {
    let size = libc::SIGSTKSZ.max(64 << 10);
    // Leaked deliberately: the kernel holds this address for as long as the
    // thread can take a signal, which is until the process ends. Freeing it would
    // leave the kernel pointing at freed memory.
    let memory = Box::leak(vec![0_u8; size].into_boxed_slice());
    let alternate = libc::stack_t {
        ss_sp: memory.as_mut_ptr().cast(),
        ss_flags: 0,
        ss_size: size,
    };
    // SAFETY: `alternate` describes a live allocation of exactly `ss_size` bytes
    // that outlives every signal that could arrive, which is what `sigaltstack`
    // requires of it.
    if unsafe { libc::sigaltstack(&alternate, std::ptr::null_mut()) } != 0 {
        note!("mesh: could not install a signal stack; a stack overflow will abort");
    }
}

/// Catch the faults a stack overflow can arrive as.
///
/// `SIGBUS` as well as `SIGSEGV`, because which one the kernel raises for a hit
/// on the guard page is not uniform across platforms.
fn install_fault_handler() {
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        // SAFETY: a zeroed `sigaction` is the documented starting point and every
        // field is written before use. `SA_ONSTACK` is what routes the handler to
        // the alternate stack — without it the handler is as stuck as the code it
        // is meant to report on — and `SA_SIGINFO` is what makes `si_addr`
        // available, pairing with the three-argument handler installed here.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fault as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_ONSTACK | libc::SA_SIGINFO;
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                note!("mesh: could not catch fatal faults; a stack overflow will abort");
                return;
            }
        }
    }
}

/// What a fault prints. Held as constants because a handler may neither allocate
/// nor format, so the wording has to be settled before the fault.
///
/// The terminal variants lead with a carriage return and end their lines with
/// one: an interactive shell is in raw mode while it reads, where a bare newline
/// drops a row without returning to the first column and leaves the message
/// stepped diagonally across the screen.
///
/// The fallback pair does not claim a bug, because it covers two situations and
/// only one of them is: a fault genuinely outside the stack, and a fault whose
/// address could not be *judged* because the bounds are unknown — which is what
/// `ulimit -s unlimited` leaves, since there is then no size to measure against.
/// An overflow under an unlimited stack would land here, so wording this as
/// "a bug in mesh" would send a reader hunting for one that is not there.
const OVERFLOW_MESSAGE: &str = "mesh: fatal: out of stack, most likely from input nested \
                                or recursing too deeply\n";
const OVERFLOW_MESSAGE_TTY: &str = "\r\nmesh: fatal: out of stack, most likely from input \
                                    nested or recursing too deeply\r\n";
const FAULT_MESSAGE: &str = "mesh: fatal: invalid memory access or out of stack; \
                             the shell cannot continue\n";
const FAULT_MESSAGE_TTY: &str = "\r\nmesh: fatal: invalid memory access or out of stack; \
                                 the shell cannot continue\r\n";

/// Report, then leave. Runs on the alternate stack, using only the two calls a
/// signal handler is allowed to make here.
///
/// `_exit` rather than returning or calling `exit`: returning would retry the
/// faulting instruction and fault again forever, and `exit` runs destructors and
/// `atexit` handlers that are not safe to run from a signal.
extern "C" fn on_fault(
    _signal: libc::c_int,
    info: *mut libc::siginfo_t,
    _context: *mut libc::c_void,
) {
    // A bounded run supervised from this process has its command in a process
    // group nothing else can name, so ending here without passing the fault on
    // would leave that command running with no deadline behind it. Sent first,
    // because everything below this leads to `_exit`.
    //
    // SAFETY: async-signal-safe (one atomic read and a `kill`), and this handler
    // is only reached in a process that could be supervising one.
    unsafe { exec::hand_off_timed_group(libc::SIGKILL) };
    // SAFETY: `SA_SIGINFO` guarantees a valid `siginfo_t` for the life of the
    // handler, and `si_addr` only reads it.
    let address = unsafe { info.as_ref().map_or(0, |info| info.si_addr() as usize) };
    let low = STACK_LOW.load(Ordering::Relaxed);
    let high = STACK_HIGH.load(Ordering::Relaxed);
    // The bounds belong to the thread that installed them, so a fault on any
    // other thread falls through to the general message rather than claiming an
    // overflow it has no way to know about.
    let overflowed = low != 0 && address >= low.saturating_sub(GUARD_SLACK) && address < high;
    let message = match (overflowed, STDERR_IS_TERMINAL.load(Ordering::Relaxed)) {
        (true, true) => OVERFLOW_MESSAGE_TTY,
        (true, false) => OVERFLOW_MESSAGE,
        (false, true) => FAULT_MESSAGE_TTY,
        (false, false) => FAULT_MESSAGE,
    };
    // SAFETY: writing a static string to fd 2, then leaving. The result of the
    // write is deliberately unused: there is nowhere left to report a failure to
    // report, and the exit has to happen either way.
    unsafe {
        libc::write(2, message.as_ptr().cast(), message.len());
        libc::_exit(EXIT_FAULT as libc::c_int);
    }
}
