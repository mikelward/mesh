//! The one place that calls `waitpid`.
//!
//! Job state used to be learned by *polling*: whatever wanted to know asked
//! `waitpid` about the pids it cared about, whenever it happened to be asked.
//! That makes state only as fresh as the last command boundary, and it loses the
//! order of changes that were noticed in the same pass — the kernel reports
//! *that* a child stopped, never when, so two stops found together could only be
//! ordered by however the table happened to hold them.
//!
//! So reaping moves behind one owner. [`drain`] takes every transition the
//! kernel is holding for a child the shell owns and files it in [`Reaper`],
//! stamped with the order it arrived. Everything that used to call `waitpid`
//! reads from here.
//!
//! Two properties this arrangement has to keep, both of which ruled out the
//! obvious `waitpid(-1)` sprinkled at call sites:
//!
//! * **Nothing steals.** A blocking wait for one job must not lose its
//!   notification to a drain running for another. Since transitions are *stored*
//!   rather than consumed on sight, a waiter finds what a drain took on its
//!   behalf.
//! * **The handler only records.** Looking at the table must never change what
//!   the shell does, so the `SIGCHLD` handler does no work at all — it only
//!   forwards itself to whichever thread is waiting, and the real work happens
//!   there.
//!
//! ## Waiting
//!
//! A wait wants to notice *every* job while it sleeps, not only the one it is
//! waiting for, so their transitions are filed in the order they happen rather
//! than all at once afterwards. Blocking in `waitpid` cannot do that safely: a
//! `SIGCHLD` arriving between the drain and the syscall is handled and gone,
//! leaving the waiter asleep with an undrained transition behind it.
//!
//! So [`WaitCatcher`] holds `SIGCHLD` blocked on the waiting thread, the loop
//! drains and checks the store with it held, and [`wait_for_change`] hands it
//! over to `sigsuspend` — unblock-and-wait as one operation, with no instant
//! where the signal is deliverable and the thread is not yet waiting. A signal
//! that arrived while the wait was preparing is pending, and lands the moment
//! the mask lifts.
//!
//! The handler `pthread_kill`s the waiting thread, the way `exec::SigintCatcher`
//! already did for SIGINT: mesh is multi-threaded — `$(…)` and `:capture` run a
//! reader thread — so a process-directed `SIGCHLD` reaches whichever thread does
//! not block it, which while a wait is installed is never the waiter.
//!
//! A self-pipe would also work and was what this did first. It cost seven
//! review findings, because a pipe is a *descriptor* and mesh lets a script name
//! any descriptor it likes: the endpoint answered `>&3`, then `>&101`, then
//! collided with relocation targets, then could not relocate at all under a low
//! `RLIMIT_NOFILE`, then blocked the drain, and finally turned out to be
//! reachable by path through `/dev/fd/100`. `pthread_kill` needs no namespace at
//! all, and is what the other shells' equivalent machinery amounts to.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// A state change the kernel reported for one child, and where it sits in the
/// order they arrived.
#[derive(Clone, Copy)]
pub(crate) struct Transition {
    /// The raw `waitpid` status, classified by the caller exactly as it was when
    /// the caller made the call itself.
    pub(crate) raw: libc::c_int,
    /// A global sequence number, in the order the drain *took* the transitions —
    /// which is the order they happened only as far as the drain was able to
    /// interleave with them.
    ///
    /// A wait is cut short per state change, so a shell waiting through two of
    /// them takes them in order and recency follows the stops rather than the
    /// table. Two that land in the same scheduling interval do not: they are
    /// already both pending when the drain starts, and nothing distinguishes
    /// them. That case stays as arbitrary as it was, because nothing portable
    /// records *when* a child stopped — `TODO.md` keeps it.
    pub(crate) seq: u64,
}

#[derive(Default)]
struct Reaper {
    /// Per pid, in arrival order. A queue rather than a slot because a job can
    /// stop, continue and exit between two drains, and a reader that took only
    /// the first would report the wrong thing about what it lived through.
    events: HashMap<libc::pid_t, VecDeque<Transition>>,
    /// Pids the shell spawned. Nothing else is ever waited for: a child of some
    /// other owner — the completion helper waits on its own with `Child::wait` —
    /// is not the shell's to reap, and a parent's child inherited across `exec`
    /// is not the shell's to block on.
    owned: std::collections::HashSet<libc::pid_t>,
    /// Owned pids whose statuses nobody will ever ask for. A background command
    /// inside a forked stage is the case: the stage still has to reap it, or it
    /// leaves a zombie, but the `Outcome` that would have read the status is
    /// dropped when the stage returns. Filed anyway, those statuses accumulate
    /// for the life of the stage and — once the kernel reuses a pid — hand a
    /// later child the wrong one.
    discarded: std::collections::HashSet<libc::pid_t>,
    next_seq: u64,
}

fn reaper() -> &'static Mutex<Reaper> {
    static REAPER: OnceLock<Mutex<Reaper>> = OnceLock::new();
    REAPER.get_or_init(|| {
        guard_the_store_across_forks();
        Mutex::default()
    })
}

/// The store, held from before a `fork` until after it, so that no child ever
/// inherits it locked.
///
/// `fork` copies the lock but not the thread holding it. A child that inherits a
/// held store waits on a thread that does not exist in it, forever — and the
/// first thing a forked child does is [`forget_all`]. The parent's blocking
/// `waitpid` then never returns either, which is how one stranded child hangs a
/// whole test run.
///
/// Taking the lock in the `prepare` handler makes the fork wait for whoever
/// holds it, so the copy the child gets is always free. Both sides then release
/// it: the handlers run on the thread that called `fork`, which is the thread
/// that took it, so the guard is dropped by its own owner in each process.
static HELD_ACROSS_FORK: AtomicPtr<MutexGuard<'static, Reaper>> =
    AtomicPtr::new(std::ptr::null_mut());

extern "C" fn take_store_for_fork() {
    // A poisoned store still has to be held across the fork; the panic that
    // poisoned it says nothing about whether a child may inherit the lock.
    let guard = reaper().lock().unwrap_or_else(|held| held.into_inner());
    HELD_ACROSS_FORK.store(Box::into_raw(Box::new(guard)), Ordering::SeqCst);
}

extern "C" fn release_store_after_fork() {
    let held = HELD_ACROSS_FORK.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !held.is_null() {
        // SAFETY: the pointer came from `Box::into_raw` in the `prepare` handler
        // and is taken exactly once — the swap leaves null behind, so the parent
        // and the child each free their own copy.
        drop(unsafe { Box::from_raw(held) });
    }
}

/// Registered from the store's initializer rather than from [`install`], since a
/// test forks without ever installing the handler and needs the same protection.
fn guard_the_store_across_forks() {
    // SAFETY: the three handlers are `extern "C" fn()` with no arguments, and
    // none of them can re-enter this function: `reaper()` is mid-initialization
    // here, and the handlers only run on a later `fork`.
    unsafe {
        libc::pthread_atfork(
            Some(take_store_for_fork),
            Some(release_store_after_fork),
            Some(release_store_after_fork),
        );
    }
}

/// The thread whose `waitpid` a `SIGCHLD` should cut short, as a `pthread_t`.
/// Zero when no wait is in progress.
static WAITING_THREAD: AtomicUsize = AtomicUsize::new(0);

/// Whether [`install`] has run.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Forward to the waiting thread, and otherwise do nothing at all.
///
/// Async-signal-safe by being trivial: `pthread_self`, `pthread_equal` and
/// `pthread_kill` are all on the list, and nothing here allocates, locks, or
/// touches the table. Every decision about what a transition *means* happens in
/// [`drain`], in ordinary code.
extern "C" fn note_child(signal: libc::c_int) {
    let waiting = WAITING_THREAD.load(Ordering::SeqCst);
    if waiting == 0 {
        return;
    }
    // SAFETY: `waiting` is a `pthread_t` published by `WaitCatcher::install` for
    // a thread blocked in the wait it belongs to, and cleared before that thread
    // returns. The equality test stops the forward from recurring once it
    // arrives.
    unsafe {
        let waiter = waiting as libc::pthread_t;
        if libc::pthread_equal(libc::pthread_self(), waiter) == 0 {
            libc::pthread_kill(waiter, signal);
        }
    }
}

/// Take `SIGCHLD` from whatever the shell inherited, once, at startup.
///
/// Two things have to be true before any child is spawned, and neither is about
/// waking anybody:
///
/// * The disposition must not be `SIG_IGN`. A shell started with `SIGCHLD`
///   ignored has its children auto-reaped by the kernel, so no status ever
///   reaches the store and a wait sits for news that cannot come — `mesh -c true`
///   hung outright that way.
/// * The signal must not be blocked. An inherited mask is not the shell's to
///   keep: with `SIGCHLD` blocked the handler never runs, so a wait is never cut
///   short and every job's transition arrives late.
///
/// `SA_RESTART` here, deliberately: outside a wait nothing wants a `SIGCHLD` to
/// fail a blocking read, least of all the line editor at the prompt.
/// [`WaitCatcher`] is what drops it for the span of a wait.
pub(crate) fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: a zeroed `sigaction` is the documented starting point, and
    // `note_child` has the C handler signature.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = note_child as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        // No `SA_NOCLDSTOP`: a stop is one of the transitions this exists to
        // hear about.
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut());

        let mut only_child: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut only_child);
        libc::sigaddset(&mut only_child, libc::SIGCHLD);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &only_child, std::ptr::null_mut());
    }
}

/// Hold `SIGCHLD` for the span of a wait, so the wait can check and sleep
/// without a state change slipping between the two.
///
/// The signal is **blocked on the waiting thread** for as long as this lives,
/// and [`wait_for_change`] is the only thing that lets it through — atomically,
/// via `sigsuspend`. That is what closes the window a blocking `waitpid` cannot:
/// with `waitpid`, a `SIGCHLD` arriving after the drain and before the syscall
/// is handled and gone, leaving the waiter asleep with an undrained transition
/// sitting behind it, and a long foreground wait then collapses the arrival
/// order it exists to preserve. Blocked, that signal stays *pending* and is
/// delivered the moment `sigsuspend` unblocks it.
///
/// The published thread is for the other half: mesh runs a reader thread for
/// `$(…)` and `:capture`, so a process-directed `SIGCHLD` reaches whichever
/// thread does not block it — which, while this is installed, is never the
/// waiter. [`note_child`] forwards it on with `pthread_kill`, and the waiter
/// finds it pending exactly as if it had arrived there directly.
pub(crate) struct WaitCatcher {
    previous_mask: libc::sigset_t,
}

impl WaitCatcher {
    pub(crate) fn install() -> Option<Self> {
        // The handler has to exist before anything sleeps on it. `sigsuspend`
        // returns when a *handler* runs, and `SIGCHLD`'s default action is to be
        // discarded — so a wait reached without [`install`] having run sleeps
        // through every child it is waiting for. The shell calls it at startup,
        // but leaving it at that made being woken depend on which door the code
        // came in by, and a unit test that reaches a wait directly hung on it.
        // Sleeping is now sufficient for being woken.
        install();
        // SAFETY: a zeroed `sigset_t` is the documented starting point, and the
        // mask changes affect only the calling thread.
        unsafe {
            let mut only_child: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut only_child);
            libc::sigaddset(&mut only_child, libc::SIGCHLD);
            let mut previous_mask: libc::sigset_t = std::mem::zeroed();
            if libc::pthread_sigmask(libc::SIG_BLOCK, &only_child, &mut previous_mask) != 0 {
                return None;
            }
            // Published after the block, so a handler cannot forward to a thread
            // that has not yet arranged to hold what it is sent.
            WAITING_THREAD.store(libc::pthread_self() as usize, Ordering::SeqCst);
            Some(Self { previous_mask })
        }
    }
}

impl Drop for WaitCatcher {
    fn drop(&mut self) {
        // Cleared before the mask goes back, so nothing forwards to a wait that
        // has ended and left the signal unblocked to act on.
        WAITING_THREAD.store(0, Ordering::SeqCst);
        // SAFETY: `previous_mask` was filled in by `pthread_sigmask` on this
        // same thread.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous_mask, std::ptr::null_mut());
        }
    }
}

/// Sleep until a child changes state, or a signal says to stop waiting.
///
/// `sigsuspend` unblocks `SIGCHLD` and waits as one operation, which is the
/// whole point: there is no instant where the signal is deliverable and this
/// thread is not yet waiting for it. A `SIGCHLD` that arrived while the wait was
/// preparing is pending, and lands the moment the mask lifts.
///
/// Returns when *any* handled signal arrives — a SIGINT while `SigintCatcher` is
/// installed also ends the sleep, which is what lets a Ctrl-C out of a wait.
pub(crate) fn wait_for_change() {
    // SAFETY: reading the current mask and suspending on a copy of it; both
    // affect only this thread, and `sigsuspend` always returns `EINTR`.
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        if libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask) != 0 {
            return;
        }
        libc::sigdelset(&mut mask, libc::SIGCHLD);
        libc::sigsuspend(&mask);
    }
}

/// Take note that a pid belongs to the shell, so the drain may reap it.
pub(crate) fn own(pid: libc::pid_t) {
    if let Ok(mut reaper) = reaper().lock() {
        reaper.owned.insert(pid);
    }
}

/// Forget everything. Called in a forked child, which inherits a copy of the
/// table and of this store but is not the parent of anything in either — its
/// `waitpid` would fail with `ECHILD`, and a transition recorded before the fork
/// describes a process this child has no business reporting on.
pub(crate) fn forget_all() {
    if let Ok(mut reaper) = reaper().lock() {
        reaper.events.clear();
        reaper.owned.clear();
        reaper.discarded.clear();
    }
}

/// File a transition, stamping it with its place in the arrival order.
///
/// Everything goes through here, including a status a blocking wait collected
/// itself, so there is one place that decides ordering and one place that
/// retires a pid.
pub(crate) fn record(pid: libc::pid_t, raw: libc::c_int) {
    let Ok(mut reaper) = reaper().lock() else {
        return;
    };
    if reaper.discarded.contains(&pid) {
        // Reaped so it does not linger, and forgotten so it cannot accumulate
        // or be mistaken for a later child that inherits the number.
        if libc::WIFEXITED(raw) || libc::WIFSIGNALED(raw) {
            reaper.owned.remove(&pid);
            reaper.discarded.remove(&pid);
        }
        return;
    }
    let seq = reaper.next_seq;
    reaper.next_seq += 1;
    reaper
        .events
        .entry(pid)
        .or_default()
        .push_back(Transition { raw, seq });
    // A pid that has exited will never report again, and leaving it owned would
    // keep a dead entry for every command the shell ever ran.
    if libc::WIFEXITED(raw) || libc::WIFSIGNALED(raw) {
        reaper.owned.remove(&pid);
    }
}

/// Keep reaping a pid, but throw its statuses away.
///
/// For a child nothing will ever ask about. Dropping the claim instead would
/// leave a zombie, since a reaped child is the only kind that goes away; filing
/// the status instead is what grows the store without bound.
pub(crate) fn abandon(pid: libc::pid_t) {
    {
        let Ok(mut reaper) = reaper().lock() else {
            return;
        };
        if reaper.owned.contains(&pid) {
            reaper.discarded.insert(pid);
        }
        // Anything already filed for it is equally unreachable.
        reaper.events.remove(&pid);
    }
    // And collect whatever has finished since the last one was abandoned.
    //
    // Marking alone is not enough, because the process doing the abandoning may
    // never wait for anything again. A forked stage that only launches
    // background commands — `fork { loop { sh -c … &; x = 1 } }` — reaches no
    // foreground wait and no `jobs`, so nothing else ever calls `drain` and the
    // children pile up as zombies until the stage hits its process limit.
    // Draining here makes each launch collect the previous ones, which bounds
    // the pile at however many are still genuinely running.
    drain();
}

/// Stop expecting anything further from a pid.
pub(crate) fn disown(pid: libc::pid_t) {
    if let Ok(mut reaper) = reaper().lock() {
        reaper.owned.remove(&pid);
        reaper.discarded.remove(&pid);
    }
}

/// Move every transition the kernel is holding into the store.
///
/// Walks the pids the shell **owns** and asks about each by name. Discovering
/// them instead with `waitid(P_ALL, …, WNOWAIT)` was what this did first, and it
/// deadlocks: mesh can be exec'd by a process that already has a live child, and
/// once that child has a pending transition every `P_ALL` probe answers with the
/// same unowned pid — `WNOWAIT` leaves it pending, so there is no way past it.
///
/// Asking by name costs one syscall per job rather than one per event, and
/// cannot see a child that is not the shell's business at all.
pub(crate) fn drain() {
    let owned: Vec<libc::pid_t> = {
        let Ok(reaper) = reaper().lock() else {
            return;
        };
        reaper.owned.iter().copied().collect()
    };
    for pid in owned {
        // Repeated per pid: a job can have stopped *and* been continued since
        // the last drain, and both are waiting.
        loop {
            let mut raw = 0;
            // SAFETY: `raw` is writable and `pid` is a child this shell spawned.
            // `WNOHANG` so a job with nothing to report costs one call.
            let taken = unsafe {
                libc::waitpid(
                    pid,
                    &mut raw,
                    libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
                )
            };
            if taken != pid {
                // `ECHILD` means it is not ours to wait for at all — reaped by a
                // kernel that had `SIGCHLD` ignored when the shell started, say.
                // Dropping the claim keeps a waiter from sleeping for news that
                // can never come.
                if taken < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
                {
                    disown(pid);
                }
                break;
            }
            let exited = libc::WIFEXITED(raw) || libc::WIFSIGNALED(raw);
            record(pid, raw);
            if exited {
                break;
            }
        }
    }
}

/// The next recorded transition for `pid`, in the order the kernel reported it.
pub(crate) fn take(pid: libc::pid_t) -> Option<Transition> {
    let mut reaper = reaper().lock().ok()?;
    let queue = reaper.events.get_mut(&pid)?;
    let next = queue.pop_front();
    if queue.is_empty() {
        reaper.events.remove(&pid);
    }
    next
}

/// Whether the shell still expects to hear about this pid. False once it has
/// exited and its last transition has been taken, which is how a waiter tells
/// "nothing yet" from "nothing ever again".
pub(crate) fn is_owned(pid: libc::pid_t) -> bool {
    reaper()
        .lock()
        .map(|reaper| reaper.owned.contains(&pid))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// A child forked while another thread holds the store must still be able to
    /// reach it.
    ///
    /// The test harness runs tests on parallel threads, so a test that forks
    /// does it from a process where another thread may be inside `reaper()`.
    /// `fork` carries over the lock but not the thread holding it, so the
    /// child's copy is held by nobody and `forget_all` — the first thing a
    /// forked child calls — waits on it forever. The parent's blocking `waitpid`
    /// then never returns, which is the whole-suite hang in `TODO.md`.
    #[test]
    fn a_child_forked_while_the_store_is_held_can_still_reach_it() {
        let (locked, held) = mpsc::channel();
        let holder = thread::spawn(move || {
            let guard = reaper().lock().expect("hold the store");
            locked.send(()).expect("announce the lock");
            // Long enough for the fork below to be underway while the store is
            // still held — the window being tested. The holder releases on its
            // own timeline, the way real code does; waiting for the fork instead
            // would deadlock against a `prepare` handler that waits for the
            // store, which is a cycle no caller of `fork` actually forms.
            thread::sleep(Duration::from_millis(500));
            drop(guard);
        });
        held.recv().expect("the store is held");

        // SAFETY: `fork` takes no arguments. The child only calls `forget_all`
        // and leaves through `_exit`, so it never unwinds into the harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
        if pid == 0 {
            forget_all();
            // SAFETY: leaving without running the parent's destructors is the
            // point; this child owns none of them.
            unsafe { libc::_exit(0) };
        }
        holder.join().expect("the holder thread");

        // Without the `prepare` handler the fork goes through while the store is
        // held, and the child's own copy stays held by nobody: it never comes
        // back from `forget_all`. The deadline is what tells that hang from a
        // slow start — it is not a retry papering one over.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = 0;
        let mut reaped = 0;
        while Instant::now() < deadline {
            // SAFETY: scalar arguments and a stack slot for the status.
            reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if reaped == 0 {
            // SAFETY: scalar arguments; the pid is this process's own child.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                let mut discard = 0;
                libc::waitpid(pid, &mut discard, 0);
            }
            panic!("the child never came back from a store it inherited locked");
        }
        assert_eq!(reaped, pid);
        assert!(libc::WIFEXITED(status), "the child died rather than exited");
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}
